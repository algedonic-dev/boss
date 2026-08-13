//! Station registry — stations.md (Q1–Q4 ratified 2026-08-13).
//!
//! A **station** is an abstract priority queue that routes or holds
//! job-packet traffic until there is bandwidth or capability to
//! handle the packet. Everything about a station is registry data —
//! never a code path. Membership is DERIVED: the row's `predicate`
//! is evaluated over open Jobs at read time (see
//! [`crate::station_queue`]); there is no mutable current-station
//! field on the packet.
//!
//! Shape mirrors `StepPluginRegistry` / `WorkflowRegistry`:
//! append-only versioning + a status lifecycle (draft → active →
//! retired), a partial unique index enforcing at most one active row
//! per name, and every write recording its event atomically with the
//! row (the workflow-registry-events posture).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::WorkflowStatus;
use crate::station_queue::{DisciplineKey, StationPredicate, default_discipline};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The ratified station taxonomy (stations.md): all rows, one
/// registry. The variants are the spec's own closed vocabulary —
/// which flavors of queue exist — not a tenant-extensible taxonomy
/// (those go through the Class registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StationKind {
    /// Every executor has one — the personal queue (My Day rendered).
    Actor,
    /// Served by a set of actors: departments, teams.
    Group,
    /// Membership defined by capability predicates (skills,
    /// authority, sign-off rights), not an enumerated roster.
    Constraint,
    /// The SDLC's bundling points — packets accumulate for periodic,
    /// higher-bandwidth handling (loading dock, review queue, board
    /// windows).
    Batch,
}

impl StationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Actor => "actor",
            Self::Group => "group",
            Self::Constraint => "constraint",
            Self::Batch => "batch",
        }
    }
}

impl std::str::FromStr for StationKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "actor" => Ok(Self::Actor),
            "group" => Ok(Self::Group),
            "constraint" => Ok(Self::Constraint),
            "batch" => Ok(Self::Batch),
            other => Err(format!("unknown station kind: {other}")),
        }
    }
}

/// Who may claim a packet FROM this station — Class-registry
/// vocabulary (role slugs). Checked at the claim CAS when the claim
/// names its station. Absent = any actor may claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StationCapability {
    #[serde(default)]
    pub roles: Vec<String>,
}

impl StationCapability {
    /// Whether an actor holding `role` may claim from this station.
    /// An empty roles list gates nobody out (a declared-but-empty
    /// capability is a vacuous constraint, not a lockout).
    pub fn allows_role(&self, role: &str) -> bool {
        self.roles.is_empty() || self.roles.iter().any(|r| r == role)
    }
}

/// A full station row. Serializes directly to the `stations` columns
/// with the same names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StationSpec {
    pub name: String,
    pub version: i32,
    pub status: WorkflowStatus,
    pub title: String,
    pub kind: StationKind,
    /// The queue-membership predicate over packets — see
    /// [`StationPredicate`] for the documented JSON shape.
    pub predicate: StationPredicate,
    /// Queue ordering as data; `priority, then age` default.
    #[serde(default = "default_discipline")]
    pub discipline: Vec<DisciplineKey>,
    /// Advisory bandwidth declaration; the queue envelope reports
    /// `over_limit` when the queue exceeds it.
    #[serde(default)]
    pub wip_limit: Option<i32>,
    /// Optional capability gate at the claim CAS.
    #[serde(default)]
    pub capability: Option<StationCapability>,
    /// Visual rollup grouping (team/department) — view-level clutter
    /// control, not a data-model claim.
    #[serde(default)]
    pub rollup_parent: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl StationSpec {
    /// Convenience constructor for tests + seeds.
    pub fn draft(
        name: impl Into<String>,
        title: impl Into<String>,
        kind: StationKind,
        predicate: StationPredicate,
    ) -> Self {
        Self {
            name: name.into(),
            version: 1,
            status: WorkflowStatus::Draft,
            title: title.into(),
            kind,
            predicate,
            discipline: default_discipline(),
            wip_limit: None,
            capability: None,
            rollup_parent: None,
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StationError {
    #[error("station not found: {0}")]
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
pub trait StationRegistry: Send + Sync {
    /// Currently-active spec for `name`.
    async fn get_active(&self, name: &str) -> Result<StationSpec, StationError>;

    /// Specific historical version.
    async fn get_version(&self, name: &str, version: i32) -> Result<StationSpec, StationError>;

    /// Every active station, name-ordered.
    async fn list_active(&self) -> Result<Vec<StationSpec>, StationError>;

    /// Every version of one name (oldest first). Includes drafts +
    /// retired.
    async fn list_versions(&self, name: &str) -> Result<Vec<StationSpec>, StationError>;

    /// Append a new draft row. Version = max(version)+1 or 1.
    ///
    /// Every write method takes `actor` + `now` — a station edit is a
    /// network configuration change, so the adapter builds the
    /// corresponding event via `events::station_registry_event` and
    /// records it atomically with the row. Records
    /// `jobs.station.draft_saved` (payload = the stored draft).
    async fn create_draft(
        &self,
        spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError>;

    /// Flip the latest draft to active and demote the previous
    /// active. Transactional; records `jobs.station.published`
    /// (payload = the promoted spec) with the row flips.
    async fn publish(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError>;

    /// Flip the active row to retired. Idempotent if already retired
    /// — and silent then too: only a write that touched a row records
    /// `jobs.station.retired` (payload = the retired spec).
    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), StationError>;
}

// ---------------------------------------------------------------------------
// In-memory adapter
// ---------------------------------------------------------------------------

pub struct InMemoryStations {
    rows: Arc<Mutex<HashMap<(String, i32), StationSpec>>>,
    /// What the Pg adapter records into `event_outbox` inside the
    /// row transaction, this adapter collects here — same events at
    /// the same write points, so tests assert the event contract
    /// through the port without a database.
    recorded: Arc<Mutex<Vec<boss_core::event::Event>>>,
}

impl Default for InMemoryStations {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStations {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(HashMap::new())),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every event a write method recorded, in write order — the
    /// in-memory stand-in for `SELECT ... FROM event_outbox`.
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().unwrap().clone()
    }

    fn record(&self, event: boss_core::event::Event) {
        self.recorded.lock().unwrap().push(event);
    }

    /// Seed helper for tests. Inserts as-is without versioning logic.
    pub fn seed(&self, spec: StationSpec) -> Result<(), StationError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&(spec.name.clone(), spec.version)) {
            return Err(StationError::Conflict(format!(
                "row already exists: {}@{}",
                spec.name, spec.version
            )));
        }
        rows.insert((spec.name.clone(), spec.version), spec);
        Ok(())
    }

    fn snapshot(&self) -> Vec<StationSpec> {
        self.rows.lock().unwrap().values().cloned().collect()
    }

    fn max_version(&self, name: &str) -> Option<i32> {
        let rows = self.rows.lock().unwrap();
        rows.keys()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .max()
    }
}

#[async_trait]
impl StationRegistry for InMemoryStations {
    async fn get_active(&self, name: &str) -> Result<StationSpec, StationError> {
        self.snapshot()
            .into_iter()
            .find(|r| r.name == name && r.status == WorkflowStatus::Active)
            .ok_or_else(|| StationError::NotFound(format!("no active station: {name}")))
    }

    async fn get_version(&self, name: &str, version: i32) -> Result<StationSpec, StationError> {
        self.rows
            .lock()
            .unwrap()
            .get(&(name.to_string(), version))
            .cloned()
            .ok_or_else(|| StationError::NotFound(format!("{name}@v{version}")))
    }

    async fn list_active(&self) -> Result<Vec<StationSpec>, StationError> {
        let mut rows: Vec<StationSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.status == WorkflowStatus::Active)
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    async fn list_versions(&self, name: &str) -> Result<Vec<StationSpec>, StationError> {
        let mut rows: Vec<StationSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.name == name)
            .collect();
        rows.sort_by_key(|r| r.version);
        Ok(rows)
    }

    async fn create_draft(
        &self,
        mut spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError> {
        let next = self.max_version(&spec.name).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = WorkflowStatus::Draft;
        spec.created_at = now;
        self.rows
            .lock()
            .unwrap()
            .insert((spec.name.clone(), spec.version), spec.clone());
        self.record(crate::events::station_registry_event(
            crate::events::STATION_DRAFT_SAVED,
            actor,
            now,
            &spec,
        ));
        Ok(spec)
    }

    async fn publish(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError> {
        let mut rows = self.rows.lock().unwrap();
        let latest_draft = rows
            .values()
            .filter(|r| r.name == name && r.status == WorkflowStatus::Draft)
            .max_by_key(|r| r.version)
            .cloned()
            .ok_or_else(|| {
                StationError::NotFound(format!("no draft to publish for station: {name}"))
            })?;

        for ((n, _), row) in rows.iter_mut() {
            if n == name && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
            }
        }
        let key = (latest_draft.name.clone(), latest_draft.version);
        let row = rows.get_mut(&key).unwrap();
        row.status = WorkflowStatus::Active;
        let promoted = row.clone();
        drop(rows);
        self.record(crate::events::station_registry_event(
            crate::events::STATION_PUBLISHED,
            actor,
            now,
            &promoted,
        ));
        Ok(promoted)
    }

    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<(), StationError> {
        let mut rows = self.rows.lock().unwrap();
        let any_active = rows
            .values()
            .any(|r| r.name == name && r.status == WorkflowStatus::Active);
        if !any_active {
            // Idempotent — nothing to do, so nothing to record.
            return Ok(());
        }
        let mut retired: Option<StationSpec> = None;
        for ((n, _), row) in rows.iter_mut() {
            if n == name && row.status == WorkflowStatus::Active {
                row.status = WorkflowStatus::Retired;
                retired = Some(row.clone());
            }
        }
        drop(rows);
        if let Some(spec) = retired {
            self.record(crate::events::station_registry_event(
                crate::events::STATION_RETIRED,
                actor,
                now,
                &spec,
            ));
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

    pub struct PgStations {
        pool: PgPool,
    }

    impl PgStations {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        name: String,
        version: i32,
        status: String,
        title: String,
        kind: String,
        predicate: serde_json::Value,
        discipline: serde_json::Value,
        wip_limit: Option<i32>,
        capability: Option<serde_json::Value>,
        rollup_parent: Option<String>,
        created_at: DateTime<Utc>,
    }

    fn row_to_spec(r: Row) -> Result<StationSpec, StationError> {
        let status = r
            .status
            .parse::<WorkflowStatus>()
            .map_err(StationError::Storage)?;
        let kind = r
            .kind
            .parse::<StationKind>()
            .map_err(StationError::Storage)?;
        let predicate: StationPredicate = serde_json::from_value(r.predicate)
            .map_err(|e| StationError::Storage(format!("stations.predicate: {e}")))?;
        let discipline: Vec<DisciplineKey> = serde_json::from_value(r.discipline)
            .map_err(|e| StationError::Storage(format!("stations.discipline: {e}")))?;
        let capability: Option<StationCapability> = r
            .capability
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| StationError::Storage(format!("stations.capability: {e}")))?;
        Ok(StationSpec {
            name: r.name,
            version: r.version,
            status,
            title: r.title,
            kind,
            predicate,
            discipline,
            wip_limit: r.wip_limit,
            capability,
            rollup_parent: r.rollup_parent,
            created_at: r.created_at,
        })
    }

    const SELECT: &str = "SELECT name, version, status, title, kind, predicate, discipline, \
                          wip_limit, capability, rollup_parent, created_at \
                          FROM stations";

    #[async_trait]
    impl StationRegistry for PgStations {
        async fn get_active(&self, name: &str) -> Result<StationSpec, StationError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 AND status = 'active'"))
                    .bind(name)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StationError::NotFound(format!("no active station: {name}")))
        }

        async fn get_version(&self, name: &str, version: i32) -> Result<StationSpec, StationError> {
            let row: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 AND version = $2"))
                    .bind(name)
                    .bind(version)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StationError::NotFound(format!("{name}@v{version}")))
        }

        async fn list_active(&self) -> Result<Vec<StationSpec>, StationError> {
            let rows: Vec<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE status = 'active' ORDER BY name"))
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn list_versions(&self, name: &str) -> Result<Vec<StationSpec>, StationError> {
            let rows: Vec<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 ORDER BY version"))
                    .bind(name)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn create_draft(
            &self,
            mut spec: StationSpec,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<StationSpec, StationError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM stations WHERE name = $1")
                    .bind(&spec.name)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;
            spec.version = max.0.map(|v| v + 1).unwrap_or(1);
            spec.status = WorkflowStatus::Draft;
            spec.created_at = now;

            sqlx::query(
                "INSERT INTO stations
                    (name, version, status, title, kind, predicate, discipline,
                     wip_limit, capability, rollup_parent, created_at)
                 VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(&spec.name)
            .bind(spec.version)
            .bind(&spec.title)
            .bind(spec.kind.as_str())
            .bind(serde_json::to_value(&spec.predicate).unwrap_or_default())
            .bind(serde_json::to_value(&spec.discipline).unwrap_or_default())
            .bind(spec.wip_limit)
            .bind(
                spec.capability
                    .as_ref()
                    .map(|c| serde_json::to_value(c).unwrap_or_default()),
            )
            .bind(&spec.rollup_parent)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;

            let event = crate::events::station_registry_event(
                crate::events::STATION_DRAFT_SAVED,
                actor,
                now,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
            Ok(spec)
        }

        async fn publish(
            &self,
            name: &str,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<StationSpec, StationError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            // Read the full draft row inside the tx — the published
            // event's payload is the promoted spec, and it must
            // describe the row THIS transaction flips (a post-commit
            // re-fetch could race a concurrent writer and record a
            // spec the flip never produced).
            let draft: Option<Row> = sqlx::query_as(&format!(
                "{SELECT} WHERE name = $1 AND status = 'draft'
                 ORDER BY version DESC LIMIT 1"
            ))
            .bind(name)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            let mut promoted = draft
                .map(row_to_spec)
                .transpose()?
                .ok_or_else(|| StationError::NotFound(format!("no draft to publish: {name}")))?;

            sqlx::query(
                "UPDATE stations SET status = 'retired'
                 WHERE name = $1 AND status = 'active'",
            )
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;

            sqlx::query(
                "UPDATE stations SET status = 'active'
                 WHERE name = $1 AND version = $2",
            )
            .bind(name)
            .bind(promoted.version)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            promoted.status = WorkflowStatus::Active;

            let event = crate::events::station_registry_event(
                crate::events::STATION_PUBLISHED,
                actor,
                now,
                &promoted,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            Ok(promoted)
        }

        async fn retire(
            &self,
            name: &str,
            actor: &boss_core::actor::ActorId,
            now: DateTime<Utc>,
        ) -> Result<(), StationError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;

            // Read the active row first — the event payload is the
            // retired spec, and the idempotent no-op path (nothing
            // active) must record nothing. At most one row can be
            // active per name (partial unique index).
            let active: Option<Row> =
                sqlx::query_as(&format!("{SELECT} WHERE name = $1 AND status = 'active'"))
                    .bind(name)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| StationError::Storage(e.to_string()))?;

            let Some(row) = active else {
                // Idempotent — nothing to do, so nothing to record.
                return Ok(());
            };
            let mut spec = row_to_spec(row)?;

            sqlx::query(
                "UPDATE stations SET status = 'retired'
                 WHERE name = $1 AND status = 'active'",
            )
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(|e| StationError::Storage(e.to_string()))?;
            spec.status = WorkflowStatus::Retired;

            let event = crate::events::station_registry_event(
                crate::events::STATION_RETIRED,
                actor,
                now,
                &spec,
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(StationError::Storage)?;

            tx.commit()
                .await
                .map_err(|e| StationError::Storage(e.to_string()))?;
            Ok(())
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgStations;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> StationSpec {
        StationSpec::draft(
            name,
            format!("Test {name}"),
            StationKind::Batch,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
        )
    }

    /// Shared write-path actor: every registry write records an
    /// event, and the event needs a who. Tests are exempt from the
    /// no-wallclock lint, so `Utc::now()` rides along at call sites.
    fn test_actor() -> boss_core::actor::ActorId {
        boss_core::actor::ActorId::Human("emp-test".into())
    }

    #[tokio::test]
    async fn create_draft_assigns_next_version() {
        let reg = InMemoryStations::new();
        let v1 = reg
            .create_draft(sample("loading-dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.status, WorkflowStatus::Draft);
        let v2 = reg
            .create_draft(sample("loading-dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
    }

    #[tokio::test]
    async fn publish_retires_previous_active() {
        let reg = InMemoryStations::new();
        reg.create_draft(sample("dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        let active = reg
            .publish("dock", &test_actor(), Utc::now())
            .await
            .unwrap();
        assert_eq!(active.status, WorkflowStatus::Active);

        reg.create_draft(sample("dock"), &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.publish("dock", &test_actor(), Utc::now())
            .await
            .unwrap();

        let v1 = reg.get_version("dock", 1).await.unwrap();
        assert_eq!(v1.status, WorkflowStatus::Retired);
        let cur = reg.get_active("dock").await.unwrap();
        assert_eq!(cur.version, 2);
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let reg = InMemoryStations::new();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
        reg.retire("never-existed", &test_actor(), Utc::now())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_active_is_name_ordered() {
        let reg = InMemoryStations::new();
        let mut b = sample("b-station");
        b.status = WorkflowStatus::Active;
        reg.seed(b).unwrap();
        let mut a = sample("a-station");
        a.status = WorkflowStatus::Active;
        reg.seed(a).unwrap();
        let mut d = sample("d-draft");
        d.status = WorkflowStatus::Draft;
        reg.seed(d).unwrap();

        let names: Vec<String> = reg
            .list_active()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(
            names,
            vec!["a-station".to_string(), "b-station".to_string()]
        );
    }

    #[tokio::test]
    async fn capability_allows_role() {
        let cap = StationCapability {
            roles: vec!["head-brewer".into(), "brewer".into()],
        };
        assert!(cap.allows_role("brewer"));
        assert!(!cap.allows_role("bookkeeper"));
        // Declared-but-empty gates nobody out.
        assert!(StationCapability::default().allows_role("anyone"));
    }

    // -----------------------------------------------------------
    // Registry events — every write records an outbox event with
    // the row (the workflow-registry posture). The InMemory adapter
    // collects what the Pg adapter records inside the row
    // transaction; `recorded_events()` is the test window.
    // -----------------------------------------------------------

    #[tokio::test]
    async fn create_draft_records_draft_saved() {
        let reg = InMemoryStations::new();
        let draft = reg
            .create_draft(sample("loading-dock"), &test_actor(), Utc::now())
            .await
            .unwrap();

        let events = reg.recorded_events();
        assert_eq!(events.len(), 1, "one write, one event");
        assert_eq!(events[0].kind, crate::events::STATION_DRAFT_SAVED);
        assert_eq!(events[0].payload["name"], "loading-dock");
        assert_eq!(events[0].payload["version"], draft.version);
        assert_eq!(events[0].payload["status"], "draft");
    }

    #[tokio::test]
    async fn publish_records_exactly_one_published_event() {
        let reg = InMemoryStations::new();
        let actor = test_actor();
        reg.create_draft(sample("loading-dock"), &actor, Utc::now())
            .await
            .unwrap();
        let published = reg
            .publish("loading-dock", &actor, Utc::now())
            .await
            .unwrap();

        let events: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::STATION_PUBLISHED)
            .collect();
        assert_eq!(
            events.len(),
            1,
            "publish must record exactly one jobs.station.published"
        );
        let payload = &events[0].payload;
        // The actor rides as `_actor` in EventStamp's exact shape —
        // a Human serializes as the bare employee id.
        assert_eq!(payload["_actor"], "emp-test");
        // The payload IS the promoted spec.
        assert_eq!(payload["name"], "loading-dock");
        assert_eq!(payload["version"], published.version);
        assert_eq!(payload["status"], "active");
    }

    #[tokio::test]
    async fn retire_records_once_and_stays_silent_when_already_retired() {
        let reg = InMemoryStations::new();
        let actor = test_actor();
        reg.create_draft(sample("loading-dock"), &actor, Utc::now())
            .await
            .unwrap();
        reg.publish("loading-dock", &actor, Utc::now())
            .await
            .unwrap();

        reg.retire("loading-dock", &actor, Utc::now())
            .await
            .unwrap();
        // Second retire is the idempotent no-op path: nothing
        // active, so no event — the log records what happened, and
        // nothing happened.
        reg.retire("loading-dock", &actor, Utc::now())
            .await
            .unwrap();

        let retired: Vec<_> = reg
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == crate::events::STATION_RETIRED)
            .collect();
        assert_eq!(
            retired.len(),
            1,
            "idempotent retire must not record a second event"
        );
        assert_eq!(retired[0].payload["name"], "loading-dock");
        assert_eq!(retired[0].payload["status"], "retired");
    }
}
