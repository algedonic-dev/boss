//! Postgres adapter for the `JobsRepository` port.

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use sqlx::PgPool;

use crate::port::{
    AssignmentRow, JobFilter, JobScope, JobsError, JobsRepository, LaunchCalendarRow,
};

pub struct PgJobs {
    pool: PgPool,
    /// Connection URL retained alongside the pool so subprocess
    /// helpers (`boss-rebuild-all` for the demo-loop reset) can
    /// reuse it without parsing config or guessing.
    db_url: Option<String>,
}

impl PgJobs {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, db_url: None }
    }

    /// Constructor variant that retains the connection URL for
    /// subprocess helpers. Falls back to `BOSS_POSTGRES_URL` env
    /// var or a hardcoded default when the URL isn't passed.
    pub fn with_url(pool: PgPool, db_url: String) -> Self {
        Self {
            pool,
            db_url: Some(db_url),
        }
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct JobRow {
    id: uuid::Uuid,
    kind: String,
    job_kind_version: i32,
    subject_kind: String,
    subject_id: String,
    title: String,
    owner_id: String,
    status: String,
    priority: String,
    opened_on: chrono::NaiveDate,
    due_on: Option<chrono::NaiveDate>,
    closed_on: Option<chrono::NaiveDate>,
    metadata: serde_json::Value,
    tags: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct StepRow {
    id: uuid::Uuid,
    job_id: uuid::Uuid,
    kind: String,
    title: String,
    assignee_id: Option<String>,
    status: String,
    sort_order: i32,
    blocked_by: Vec<uuid::Uuid>,
    sign_offs_required: serde_json::Value,
    sign_offs: serde_json::Value,
    fields: serde_json::Value,
    completed_on: Option<chrono::NaiveDate>,
    metadata: serde_json::Value,
    notes: Option<String>,
    step_plugin_version: i32,
    embedded_job: Option<uuid::Uuid>,
}

/// Joined row backing [`PgJobs::list_assignments`] — a `StepRow`
/// (flattened) plus the minimum Job context the pull surface needs.
/// `j.kind` is aliased to `job_kind` so it doesn't collide with the
/// flattened `StepRow.kind` (the step kind).
#[derive(sqlx::FromRow)]
struct AssignmentRowSql {
    #[sqlx(flatten)]
    step: StepRow,
    job_kind: String,
    subject_kind: String,
    subject_id: String,
    priority: String,
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

fn row_to_job(r: JobRow) -> Job {
    Job {
        id: JobId::from_uuid(r.id),
        kind: r.kind,
        job_kind_version: r.job_kind_version,
        subject: parse_subject(&r.subject_kind, &r.subject_id),
        title: r.title,
        owner_id: r.owner_id,
        status: parse_job_status(&r.status),
        priority: parse_priority(&r.priority),
        opened_on: r.opened_on,
        due_on: r.due_on,
        closed_on: r.closed_on,
        metadata: r.metadata,
        tags: r.tags,
    }
}

fn row_to_step(r: StepRow) -> Step {
    Step {
        id: StepId::from_uuid(r.id),
        job_id: JobId::from_uuid(r.job_id),
        kind: r.kind,
        title: r.title,
        assignee_id: r.assignee_id,
        status: parse_step_status(&r.status),
        sort_order: r.sort_order,
        blocked_by: r.blocked_by.into_iter().map(StepId::from_uuid).collect(),
        sign_offs_required: serde_json::from_value(r.sign_offs_required).unwrap_or_default(),
        sign_offs: serde_json::from_value(r.sign_offs).unwrap_or_default(),
        fields: serde_json::from_value(r.fields).unwrap_or_default(),
        completed_on: r.completed_on,
        metadata: r.metadata,
        notes: r.notes,
        step_plugin_version: r.step_plugin_version,
        embedded_job: r.embedded_job.map(JobId::from_uuid),
    }
}

/// Outbound serializer — Subject → `(subject_kind, subject_id)`
/// tuple for SQL bind on INSERT/UPDATE.
///
/// Wave 7: calls the trait's `kind()` and `id()` directly. Both
/// return slices borrowed from the Subject (Wave 7 widened
/// `kind()` from `&'static str` to `&str` so CustomSubject can
/// surface its runtime `custom_kind`).
///
/// Pairs with [`parse_subject`] below, which is the explicit
/// row → Subject entry point.
pub(crate) fn subject_parts(s: &impl boss_core::primitives::Subject) -> (&str, &str) {
    (s.kind(), s.id())
}

/// Inbound deserializer — `(subject_kind, subject_id)` columns
/// → Subject.
///
/// Trivial: Subject is a `(kind, id)` tuple, so any non-empty pair
/// round-trips unchanged — there's no kind→variant `match`.
/// Validation against the subject_kinds registry happens at write
/// time (`check_custom_subject` in the HTTP handler); reads trust
/// the row.
fn parse_subject(kind: &str, ref_id: &str) -> Subject {
    Subject::new(kind, ref_id)
}

fn parse_job_status(s: &str) -> JobStatus {
    match s {
        "draft" => JobStatus::Draft,
        "open" => JobStatus::Open,
        "blocked" => JobStatus::Blocked,
        "pending-sign-off" => JobStatus::PendingSignOff,
        "closed" => JobStatus::Closed,
        "cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Draft,
    }
}

pub(crate) fn job_status_str(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Draft => "draft",
        JobStatus::Open => "open",
        JobStatus::Blocked => "blocked",
        JobStatus::PendingSignOff => "pending-sign-off",
        JobStatus::Closed => "closed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn parse_priority(s: &str) -> Priority {
    match s {
        "emergency" => Priority::Emergency,
        "urgent" => Priority::Urgent,
        "standard" => Priority::Standard,
        "scheduled" => Priority::Scheduled,
        _ => Priority::Standard,
    }
}

pub(crate) fn priority_str(p: Priority) -> &'static str {
    match p {
        Priority::Emergency => "emergency",
        Priority::Urgent => "urgent",
        Priority::Standard => "standard",
        Priority::Scheduled => "scheduled",
    }
}

fn parse_step_status(s: &str) -> StepStatus {
    match s {
        "pending" => StepStatus::Pending,
        "ready" => StepStatus::Ready,
        "active" => StepStatus::Active,
        "completed" => StepStatus::Completed,
        "skipped" => StepStatus::Skipped,
        _ => StepStatus::Pending,
    }
}

pub(crate) fn step_status_str(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Ready => "ready",
        StepStatus::Active => "active",
        StepStatus::Completed => "completed",
        StepStatus::Skipped => "skipped",
    }
}

pub(crate) fn blocked_by_uuids(ids: &[StepId]) -> Vec<uuid::Uuid> {
    ids.iter().map(|id| *id.inner().as_uuid()).collect()
}

/// Look up the version of the currently-active step plugin for `kind`.
/// Zero when no plugin serves this kind (the step renders through an
/// in-tree surface like repair/inspection/billing), which is the same
/// value the column defaults to on INSERT without this call.
async fn active_plugin_version(pool: &sqlx::PgPool, kind: &str) -> Result<i32, JobsError> {
    let row: Option<(i32,)> =
        sqlx::query_as("SELECT version FROM step_plugins WHERE kind = $1 AND status = 'active'")
            .bind(kind)
            .fetch_optional(pool)
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
    Ok(row.map(|(v,)| v).unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl JobsRepository for PgJobs {
    async fn create_job_at(
        &self,
        job: &Job,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let (subj_kind, subj_ref) = subject_parts(&job.subject);
        // ON CONFLICT DO NOTHING — re-emission of an existing
        // Job (replay path, deterministic-UUID sim runs) is a
        // no-op rather than a 500. update_job_at is the path for
        // intentional changes; create_job_at represents "make
        // sure this Job exists with its initial state."
        sqlx::query(
            r#"
            INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id,
                              status, priority, opened_on, due_on, closed_on, metadata, tags,
                              job_kind_version, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(*job.id.inner().as_uuid())
        .bind(&job.kind)
        .bind(subj_kind)
        .bind(subj_ref)
        .bind(&job.title)
        .bind(&job.owner_id)
        .bind(job_status_str(job.status))
        .bind(priority_str(job.priority))
        .bind(job.opened_on)
        .bind(job.due_on)
        .bind(job.closed_on)
        .bind(&job.metadata)
        .bind(&job.tags)
        .bind(job.job_kind_version)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError> {
        let row = sqlx::query_as::<_, JobRow>(
            "SELECT id, kind, job_kind_version, subject_kind, subject_id, title, owner_id, status, priority, opened_on, due_on, closed_on, metadata, tags FROM jobs WHERE id = $1",
        )
        .bind(*id.inner().as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(row.map(row_to_job))
    }

    async fn update_job_at(
        &self,
        job: &Job,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let (subj_kind, subj_ref) = subject_parts(&job.subject);
        let result = sqlx::query(
            r#"
            UPDATE jobs SET kind = $2, subject_kind = $3, subject_id = $4,
                title = $5, owner_id = $6, status = $7, priority = $8,
                opened_on = $9, due_on = $10, closed_on = $11, metadata = $12,
                tags = $13, updated_at = $14
            WHERE id = $1
            "#,
        )
        .bind(*job.id.inner().as_uuid())
        .bind(&job.kind)
        .bind(subj_kind)
        .bind(subj_ref)
        .bind(&job.title)
        .bind(&job.owner_id)
        .bind(job_status_str(job.status))
        .bind(priority_str(job.priority))
        .bind(job.opened_on)
        .bind(job.due_on)
        .bind(job.closed_on)
        .bind(&job.metadata)
        .bind(&job.tags)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(JobsError::NotFound(job.id));
        }
        Ok(())
    }

    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError> {
        // Short-circuit when policy says "caller sees nothing" — no
        // point taking a DB round-trip.
        if matches!(filter.scope, JobScope::None) {
            return Ok((Vec::new(), 0));
        }

        // Static query with NULL-OR predicates — sidesteps dynamic
        // query construction. `$4` is the kind_prefix pattern already
        // formatted with a trailing %; pass NULL when no prefix.
        // Policy-scope binds ($7..$9) default to NULL / empty arrays
        // when scope is `All`, which the NULL-OR guards short-circuit.
        let prefix_pattern = filter.kind_prefix.as_ref().map(|p| format!("{p}%"));

        // Translate the scope into three mutually-exclusive parameter
        // sets. Exactly one of scope_owner / scope_owners /
        // scope_accounts is non-NULL for a scoped call; all three
        // are NULL for `All`.
        let (scope_owner, scope_owners, scope_accounts): (
            Option<&str>,
            Option<Vec<String>>,
            Option<Vec<String>>,
        ) = match &filter.scope {
            JobScope::All => (None, None, None),
            JobScope::None => unreachable!("short-circuited above"),
            JobScope::OwnerIs(u) => (Some(u.as_str()), None, None),
            JobScope::OwnerIn(us) => (None, Some(us.clone()), None),
            JobScope::AccountIn(ps) => (None, None, Some(ps.clone())),
        };

        // $10 is the caller-supplied subject_id filter (asset_id
        // or account_id from the HTTP layer). Before this bind
        // existed, the SQL ignored `filter.subject_id` entirely
        // and returned the full policy-scoped set — which is why
        // /api/jobs?account_id=foo looked empty on every detail page.
        let list_sql = r#"
            SELECT id, kind, job_kind_version, subject_kind, subject_id, title, owner_id, status,
                   priority, opened_on, due_on, closed_on, metadata, tags
            FROM jobs
            WHERE ($1::text IS NULL OR kind = $1)
              AND ($2::text IS NULL OR status = $2)
              AND ($3::text IS NULL OR owner_id = $3)
              AND ($4::text IS NULL OR kind LIKE $4)
              AND ($7::text IS NULL OR owner_id = $7)
              AND ($8::text[] IS NULL OR owner_id = ANY($8))
              AND (
                $9::text[] IS NULL
                OR (subject_kind IN ('account', 'employee')
                    AND subject_id = ANY($9))
              )
              AND ($10::text IS NULL OR subject_id = $10)
            ORDER BY opened_on DESC
            LIMIT $5 OFFSET $6
        "#;

        let rows = sqlx::query_as::<_, JobRow>(list_sql)
            .bind(filter.kind.as_deref())
            .bind(filter.status.map(job_status_str))
            .bind(filter.owner_id.as_deref())
            .bind(prefix_pattern.as_deref())
            .bind(limit)
            .bind(offset)
            .bind(scope_owner)
            .bind(scope_owners.as_deref())
            .bind(scope_accounts.as_deref())
            .bind(filter.subject_id.as_deref())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM jobs
            WHERE ($1::text IS NULL OR kind = $1)
              AND ($2::text IS NULL OR status = $2)
              AND ($3::text IS NULL OR owner_id = $3)
              AND ($4::text IS NULL OR kind LIKE $4)
              AND ($5::text IS NULL OR owner_id = $5)
              AND ($6::text[] IS NULL OR owner_id = ANY($6))
              AND (
                $7::text[] IS NULL
                OR (subject_kind IN ('account', 'employee')
                    AND subject_id = ANY($7))
              )
              AND ($8::text IS NULL OR subject_id = $8)
            "#,
        )
        .bind(filter.kind.as_deref())
        .bind(filter.status.map(job_status_str))
        .bind(filter.owner_id.as_deref())
        .bind(prefix_pattern.as_deref())
        .bind(scope_owner)
        .bind(scope_owners.as_deref())
        .bind(scope_accounts.as_deref())
        .bind(filter.subject_id.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        Ok((rows.into_iter().map(row_to_job).collect(), total))
    }

    async fn add_step_at(
        &self,
        step: &Step,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        // Snapshot the current active plugin version for this step kind
        // so republishing the plugin later doesn't retroactively change
        // which bundle the step is pinned against. Caller-supplied
        // non-zero values win (bulk replay seeding its own versions);
        // zero triggers the lookup.
        let version = if step.step_plugin_version != 0 {
            step.step_plugin_version
        } else {
            active_plugin_version(&self.pool, &step.kind).await?
        };

        // ON CONFLICT DO NOTHING for the same reason as
        // create_job_at: replay paths (deterministic sim runs)
        // re-emit Steps; update_step_at handles intentional
        // changes. Without this an idempotent retry 500's.
        sqlx::query(
            r#"
            INSERT INTO steps (id, job_id, kind, title, assignee_id, status, sort_order,
                               blocked_by, sign_offs_required, sign_offs, fields,
                               completed_on, metadata, notes, step_plugin_version,
                               embedded_job, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $17)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(*step.id.inner().as_uuid())
        .bind(*step.job_id.inner().as_uuid())
        .bind(&step.kind)
        .bind(&step.title)
        .bind(&step.assignee_id)
        .bind(step_status_str(step.status))
        .bind(step.sort_order)
        .bind(blocked_by_uuids(&step.blocked_by))
        .bind(serde_json::to_value(&step.sign_offs_required).unwrap_or_default())
        .bind(serde_json::to_value(&step.sign_offs).unwrap_or_default())
        .bind(serde_json::to_value(&step.fields).unwrap_or_default())
        .bind(step.completed_on)
        .bind(&step.metadata)
        .bind(&step.notes)
        .bind(version)
        .bind(step.embedded_job.map(|j| *j.inner().as_uuid()))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError> {
        let row = sqlx::query_as::<_, StepRow>(
            "SELECT id, job_id, kind, title, assignee_id, status, sort_order, blocked_by, sign_offs_required, sign_offs, fields, completed_on, metadata, notes, step_plugin_version, embedded_job FROM steps WHERE id = $1",
        )
        .bind(*id.inner().as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(row.map(row_to_step))
    }

    async fn update_step_at(
        &self,
        step: &Step,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let result = sqlx::query(
            r#"
            UPDATE steps SET kind = $2, title = $3, assignee_id = $4,
                -- Terminal statuses are immutable at the row (the
                -- state-machine invariant): a write whose merge was
                -- computed against a pre-completion fetch (dispatcher
                -- assign retries, JetStream redeliveries, any racing
                -- read-modify-write) cannot demote a Completed/Skipped
                -- step back to live.
                status = CASE
                    WHEN status IN ('completed', 'skipped') THEN status
                    ELSE $5
                END,
                completed_on = CASE
                    WHEN status IN ('completed', 'skipped') THEN completed_on
                    ELSE $8
                END,
                sort_order = $6, blocked_by = $7,
                metadata = CASE
                    WHEN status IN ('completed', 'skipped') THEN metadata
                    ELSE $9
                END,
                notes = $10, embedded_job = $11, updated_at = $12
            WHERE id = $1
            "#,
        )
        .bind(*step.id.inner().as_uuid())
        .bind(&step.kind)
        .bind(&step.title)
        .bind(&step.assignee_id)
        .bind(step_status_str(step.status))
        .bind(step.sort_order)
        .bind(blocked_by_uuids(&step.blocked_by))
        .bind(step.completed_on)
        .bind(&step.metadata)
        .bind(&step.notes)
        .bind(step.embedded_job.map(|j| *j.inner().as_uuid()))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(JobsError::StepNotFound(step.id));
        }
        Ok(())
    }

    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), JobsError> {
        let result = sqlx::query(
            "UPDATE steps SET sign_offs = sign_offs || $2::jsonb, updated_at = $3 \
             WHERE id = $1",
        )
        .bind(*step_id.inner().as_uuid())
        .bind(serde_json::to_value(stamp).map_err(|e| JobsError::Storage(e.to_string()))?)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(JobsError::StepNotFound(*step_id));
        }
        Ok(())
    }

    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError> {
        let rows = sqlx::query_as::<_, StepRow>(
            "SELECT id, job_id, kind, title, assignee_id, status, sort_order, blocked_by, sign_offs_required, sign_offs, fields, completed_on, metadata, notes, step_plugin_version, embedded_job FROM steps WHERE job_id = $1 ORDER BY sort_order",
        )
        .bind(*job_id.inner().as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(row_to_step).collect())
    }

    async fn list_assignments(
        &self,
        assignee_id: Option<&str>,
        roles: &[String],
        limit: i64,
    ) -> Result<Vec<AssignmentRow>, JobsError> {
        // One indexed JOIN: open Jobs × their workable steps, filtered
        // to (assigned-to-me) OR (unassigned with a role I hold). The
        // `authority_role` lives in step metadata JSONB. Ordered by
        // (opened_on, sort_order) for a stable executor queue.
        let rows = sqlx::query_as::<_, AssignmentRowSql>(
            "SELECT s.id, s.job_id, s.kind, s.title, s.assignee_id, s.status, \
                    s.sort_order, s.blocked_by, s.sign_offs_required, s.sign_offs, \
                    s.fields, s.completed_on, s.metadata, s.notes, \
                    s.step_plugin_version, s.embedded_job, \
                    j.kind AS job_kind, j.subject_kind, j.subject_id, j.priority \
             FROM steps s \
             JOIN jobs j ON s.job_id = j.id \
             WHERE j.status = 'open' \
               AND s.status IN ('ready', 'active') \
               AND ( \
                     ($1::text IS NOT NULL AND s.assignee_id = $1) \
                  OR ( (s.metadata ->> 'authority_role') = ANY($2) \
                       AND (s.assignee_id IS NULL OR s.status = 'active') ) \
               ) \
             ORDER BY j.opened_on, s.sort_order \
             LIMIT $3",
        )
        .bind(assignee_id)
        .bind(roles)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| AssignmentRow {
                job_id: JobId::from_uuid(r.step.job_id),
                job_kind: r.job_kind,
                subject_kind: r.subject_kind,
                subject_id: r.subject_id,
                priority: parse_priority(&r.priority),
                step: row_to_step(r.step),
            })
            .collect())
    }

    async fn list_assigned_workable(&self, limit: i64) -> Result<Vec<AssignmentRow>, JobsError> {
        // One indexed JOIN: open Jobs × their workable steps that already
        // carry an assignee — the whole assigned backlog in a single
        // round-trip. The sim workforce pulls this each pass and drives
        // every assigned step, decoupled from who assigned it.
        let rows = sqlx::query_as::<_, AssignmentRowSql>(
            "SELECT s.id, s.job_id, s.kind, s.title, s.assignee_id, s.status, \
                    s.sort_order, s.blocked_by, s.sign_offs_required, s.sign_offs, \
                    s.fields, s.completed_on, s.metadata, s.notes, \
                    s.step_plugin_version, s.embedded_job, \
                    j.kind AS job_kind, j.subject_kind, j.subject_id, j.priority \
             FROM steps s \
             JOIN jobs j ON s.job_id = j.id \
             WHERE j.status = 'open' \
               AND s.status IN ('ready', 'active') \
               AND s.assignee_id IS NOT NULL AND s.assignee_id <> '' \
             ORDER BY j.opened_on, s.sort_order \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| AssignmentRow {
                job_id: JobId::from_uuid(r.step.job_id),
                job_kind: r.job_kind,
                subject_kind: r.subject_kind,
                subject_id: r.subject_id,
                priority: parse_priority(&r.priority),
                step: row_to_step(r.step),
            })
            .collect())
    }

    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM steps \
             WHERE kind = $1 AND status IN ('pending', 'ready', 'active')",
        )
        .bind(step_kind)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(n)
    }

    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i64)>, JobsError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, COUNT(*)::BIGINT FROM jobs \
             WHERE ($1::text IS NULL OR status = $1) \
             GROUP BY kind ORDER BY kind",
        )
        .bind(status.map(job_status_str))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows)
    }

    async fn jobs_tier_distribution(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i32, i64)>, JobsError> {
        // Per-job tier = min(sort_order) over non-done steps, or -1
        // when every step is terminal. One CTE pass so we don't walk
        // the steps table twice. Ordered so the frontend receives a
        // deterministic serialization.
        let rows: Vec<(String, i32, i64)> = sqlx::query_as(
            r#"
            WITH per_job AS (
              SELECT j.id,
                     j.kind,
                     COALESCE(
                       MIN(s.sort_order) FILTER (
                         WHERE s.status IN ('pending', 'ready', 'active')
                       ),
                       -1
                     ) AS tier
              FROM jobs j
              LEFT JOIN steps s ON s.job_id = j.id
              WHERE ($1::text IS NULL OR j.status = $1)
              GROUP BY j.id, j.kind
            )
            SELECT kind, tier, COUNT(*)::BIGINT
            FROM per_job
            GROUP BY kind, tier
            ORDER BY kind, tier
            "#,
        )
        .bind(status.map(job_status_str))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows)
    }

    async fn list_launch_calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Vec<LaunchCalendarRow>, JobsError> {
        // Every open/pending/pending-sign-off marketing-motion joined
        // to its single launch step (the one carrying launch_date). We pull the
        // launch_date + launch_channel out of step metadata in SQL so
        // the caller doesn't have to fetch the step rows separately.
        // `current_tier` mirrors the computation in
        // `jobs_tier_distribution` (min non-done sort_order; -1 when
        // everything is terminal).
        //
        // The date window is applied inclusively on both ends. Motions
        // whose launch step has no date yet (launch_date IS NULL) are
        // intentionally returned — the UI buckets them under
        // "unscheduled" at the top of the list.
        #[derive(sqlx::FromRow)]
        struct Row {
            id: uuid::Uuid,
            title: String,
            owner_id: String,
            subject_id: String,
            status: String,
            current_tier: Option<i32>,
            launch_date: Option<chrono::NaiveDate>,
            launch_channel: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as::<_, Row>(
            r#"
            WITH launches AS (
              SELECT
                s.job_id,
                -- jsonb -> text then cast to date is tolerant of both
                -- string values ("2026-05-15") and missing keys.
                NULLIF(s.metadata ->> 'launch_date', '')::date AS launch_date,
                NULLIF(s.metadata ->> 'launch_channel', '')    AS launch_channel
              FROM steps s
              -- property, not kind: the launch step is whichever step
              -- carries a launch_date (no-step-kind-match rule)
              WHERE s.metadata ? 'launch_date'
            ),
            tiers AS (
              SELECT j.id AS job_id,
                     COALESCE(
                       MIN(s.sort_order) FILTER (
                         WHERE s.status IN ('pending','ready','active')
                       ),
                       -1
                     ) AS current_tier
              FROM jobs j
              LEFT JOIN steps s ON s.job_id = j.id
              GROUP BY j.id
            )
            SELECT j.id,
                   j.title,
                   j.owner_id,
                   j.subject_id,
                   j.status,
                   t.current_tier,
                   l.launch_date,
                   l.launch_channel
            FROM jobs j
            LEFT JOIN launches l ON l.job_id = j.id
            LEFT JOIN tiers t    ON t.job_id = j.id
            WHERE j.kind = 'marketing-motion'
              AND j.status NOT IN ('closed','cancelled')
              AND (
                l.launch_date IS NULL
                OR l.launch_date BETWEEN $1 AND $2
              )
            ORDER BY l.launch_date NULLS FIRST, j.title
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| LaunchCalendarRow {
                job_id: JobId::from_uuid(r.id),
                title: r.title,
                owner_id: Some(r.owner_id),
                subject_id: Some(r.subject_id),
                status: parse_job_status(&r.status),
                current_tier: r.current_tier,
                launch_date: r.launch_date,
                launch_channel: r.launch_channel,
            })
            .collect())
    }

    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<uuid::Uuid> = ids.iter().map(|id| *id.inner().as_uuid()).collect();

        #[derive(sqlx::FromRow)]
        struct StatusRow {
            id: uuid::Uuid,
            status: String,
        }

        let rows =
            sqlx::query_as::<_, StatusRow>("SELECT id, status FROM steps WHERE id = ANY($1)")
                .bind(&uuids)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| JobsError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| (StepId::from_uuid(r.id), parse_step_status(&r.status)))
            .collect())
    }

    async fn sim_clock_state(&self) -> Result<Option<crate::port::SimClockState>, JobsError> {
        // Single-row table by convention; LIMIT 1 belt + suspenders
        // in case a deploy ever has more than one row.
        let row: Option<(
            chrono::NaiveDate,
            Option<chrono::NaiveDate>,
            Option<chrono::NaiveDate>,
            bool,
            bool,
        )> = sqlx::query_as(
            "SELECT current_sim_date, epoch_start_date, epoch_end_date, paused, \
                    restart_in_progress \
             FROM sim_clock WHERE id = 1 LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(row.map(
            |(current_sim_date, epoch_start_date, epoch_end_date, paused, restart_in_progress)| {
                // Date-only Postgres adapter: synthesize midnight UTC for the
                // `now` field. The live SimClockState used by the SPA is built
                // via http::sim_clock_state_from_clock, which calls clock-api
                // directly and carries the full sub-day time; this date-only
                // projection is read by tests and any caller that doesn't need
                // sub-day precision.
                let now = current_sim_date
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid")
                    .and_utc();
                crate::port::SimClockState {
                    now,
                    current_sim_date,
                    epoch_start_date,
                    epoch_end_date,
                    paused,
                    restart_in_progress,
                }
            },
        ))
    }

    async fn set_sim_clock_paused(&self, paused: bool) -> Result<(), JobsError> {
        sqlx::query("UPDATE sim_clock SET paused = $1 WHERE id = 1")
            .bind(paused)
            .execute(&self.pool)
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn restart_sim_clock_epoch(&self) -> Result<(), JobsError> {
        // Read epoch_start + baseline cutoff before touching
        // anything destructive. epoch_start tells us where to
        // rewind to; baseline tells us which audit_log rows to
        // keep (everything <= it is canonical seed; everything
        // > it is live-tick accumulation).
        //
        // If epoch_baseline_audit_id is NULL (the canonical regen /
        // reset script's step 9 didn't complete OR the deploy
        // started from an older bundle that pre-dated the column),
        // self-heal by claiming MAX(audit_log.id) NOW as the
        // baseline. The user-facing effect: the first restart-epoch
        // click captures whatever audit_log state currently exists
        // as "the baseline" and rewinds future restarts to it. Not
        // ideal — the script should set this — but unblocks the
        // demo without forcing the user to ssh in.
        let row: Option<(Option<chrono::NaiveDate>, Option<i64>, bool)> = sqlx::query_as(
            "SELECT epoch_start_date, epoch_baseline_audit_id, \
                    COALESCE(restart_in_progress, false) \
             FROM sim_clock WHERE id = 1 LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        let Some((Some(start), baseline_opt, already_running)) = row else {
            return Err(JobsError::Storage(
                "sim_clock missing epoch_start_date — cannot restart epoch \
                 (run reset-to-baseline.sh once to capture the baseline)"
                    .to_string(),
            ));
        };
        let baseline = match baseline_opt {
            Some(b) => b,
            None => {
                // Self-heal: claim MAX(audit_log.id) as the baseline
                // and persist it. Subsequent restart-epoch calls hit
                // the Some(b) path.
                let max_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM audit_log")
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                sqlx::query("UPDATE sim_clock SET epoch_baseline_audit_id = $1 WHERE id = 1")
                    .bind(max_id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                tracing::warn!(
                    epoch_baseline_audit_id = max_id,
                    "sim_clock.epoch_baseline_audit_id was NULL; self-healed by capturing MAX(audit_log.id) at restart time. \
                     The reset/regen scripts should have set this — investigate which one skipped step 9."
                );
                max_id
            }
        };
        if already_running {
            return Err(JobsError::Storage(
                "restart already in progress".to_string(),
            ));
        }

        // Atomically claim the restart slot: paused=true,
        // restart_in_progress=true. The handler returns 202 to
        // the SPA right after this; the heavyweight work (truncate
        // + rebuild-all + clock-rewind) runs in a tokio::spawn
        // task. The SPA polls /api/jobs/live to see when
        // restart_in_progress flips back to false.
        sqlx::query(
            "UPDATE sim_clock \
             SET paused = true, restart_in_progress = true, updated_at = NOW() \
             WHERE id = 1",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        let pool = self.pool.clone();
        let db_url = self
            .db_url
            .clone()
            .or_else(|| std::env::var("BOSS_POSTGRES_URL").ok())
            .unwrap_or_else(|| "postgres://boss:boss@127.0.0.1/boss".to_string());
        tokio::spawn(async move {
            if let Err(e) = run_restart_epoch_background(&pool, &db_url, start, baseline).await {
                tracing::error!(error = %e, "restart_epoch background task failed");
                // Clear the flag on failure so the operator can
                // retry. Daemon stays paused; sim_clock state is
                // left in whatever state the failure produced.
                let _ =
                    sqlx::query("UPDATE sim_clock SET restart_in_progress = false WHERE id = 1")
                        .execute(&pool)
                        .await;
            }
        });

        Ok(())
    }
}

/// Heavyweight reset path — runs in a tokio::spawn task off the
/// HTTP request that triggered it. Operator polls
/// `/api/jobs/live` to see when `sim_clock.restart_in_progress`
/// flips back to false.
///
/// Trim-not-truncate strategy: the seed bundle is 880k single-row
/// INSERTs (no COPY), so re-importing it costs 5+ minutes. Instead
/// we DELETE only `audit_log` rows past the canonical baseline id
/// (the live-tick accumulation), then run the per-service
/// rebuilders to replay the surviving (smaller) audit_log into
/// fresh projections. ~30s typical.
async fn run_restart_epoch_background(
    pool: &PgPool,
    db_url: &str,
    epoch_start: chrono::NaiveDate,
    baseline: i64,
) -> Result<(), JobsError> {
    // Wait one tick interval for the daemon to observe paused=true.
    let tick_interval: i32 =
        sqlx::query_scalar("SELECT tick_interval_seconds FROM sim_clock WHERE id = 1")
            .fetch_one(pool)
            .await
            .unwrap_or(10);
    let pause_wait = std::time::Duration::from_secs((tick_interval.max(1) + 2) as u64);
    tokio::time::sleep(pause_wait).await;

    // Trim audit_log past the seed baseline. The append-only
    // trigger (DELETE rejection per the correctness-protocol
    // invariant) has to be disabled briefly — this is the one
    // controlled exception, scoped to the demo-loop reset path.
    // The per-service rebuilders below wipe their own projection
    // tables in their replay transactions, so projections clear
    // and re-derive in one pass.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| JobsError::Storage(format!("begin trim tx: {e}")))?;
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(format!("disable trigger: {e}")))?;
    let trimmed = sqlx::query("DELETE FROM audit_log WHERE id > $1")
        .bind(baseline)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(format!("trim audit_log: {e}")))?;
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(format!("re-enable trigger: {e}")))?;
    tx.commit()
        .await
        .map_err(|e| JobsError::Storage(format!("commit trim tx: {e}")))?;
    tracing::info!(
        rows_trimmed = trimmed.rows_affected(),
        baseline,
        "restart_epoch: audit_log trimmed past seed baseline"
    );

    // Spawn boss-rebuild-all WITHOUT --audit-log-seed: the
    // surviving audit_log is the source of truth; we just need
    // to re-derive projections from it.
    let rebuild_bin = std::env::var("BOSS_REBUILD_ALL_BIN")
        .unwrap_or_else(|_| "/usr/local/bin/boss-rebuild-all".to_string());
    let status = tokio::process::Command::new(&rebuild_bin)
        .arg("--database-url")
        .arg(db_url)
        .status()
        .await
        .map_err(|e| JobsError::Storage(format!("spawn {rebuild_bin}: {e}")))?;
    if !status.success() {
        return Err(JobsError::Storage(format!(
            "boss-rebuild-all exited {status}"
        )));
    }

    // Rewind the formula clock to epoch_start, unpause, clear the flag.
    // Sim-time is computed (epoch_start_date + (now − wall_anchor −
    // paused_offset) × warp), not stored — so re-anchor wall_anchor to NOW
    // with a zero pause-offset and sim_now snaps back to epoch_start_date.
    // (Writing the long-dropped current_sim_date column is what used to
    // fail this step after the formula-clock migration.) The daemon's next
    // tick resumes the new epoch against the freshly rebuilt projections.
    sqlx::query(
        "UPDATE sim_clock \
         SET epoch_start_date = $1, wall_anchor = NOW(), \
             paused_offset_seconds = 0, paused = false, \
             restart_in_progress = false, updated_at = NOW() \
         WHERE id = 1",
    )
    .bind(epoch_start)
    .execute(pool)
    .await
    .map_err(|e| JobsError::Storage(e.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests — parse_subject / subject_parts only (no DB required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::primitives::Subject as SubjectTrait;

    /// For every canonical kind, subject_parts(parse_subject(k, v))
    /// round-trips to the same (k, v) strings. This is the DB
    /// round-trip property expressed at the boundary functions,
    /// without needing a live Postgres connection.
    #[test]
    fn closed_kind_write_read_round_trip() {
        for (kind, ref_id) in [
            ("system", "SYS-001"),
            ("account", "prac-42"),
            ("purchase_order", "po-2026-0101"),
            ("campaign", "cmp-spring-26"),
            ("employee", "emp-001"),
            ("vendor", "ven-oem-coherent"),
        ] {
            let subject = parse_subject(kind, ref_id);
            let (out_kind, out_ref) = subject_parts(&subject);
            assert_eq!(out_kind, kind, "closed kind {kind:?} round-trip");
            assert_eq!(out_ref, ref_id, "closed kind {kind:?} ref_id round-trip");
        }
    }

    #[test]
    fn custom_kind_is_preserved_across_round_trip() {
        // Wave 7c preservation: a user-defined custom_kind survives
        // write → read → re-write with the real string intact.
        let subject = parse_subject("dose-review", "dr-2026-042");
        assert_eq!(SubjectTrait::kind(&subject), "dose-review");
        assert_eq!(SubjectTrait::id(&subject), "dr-2026-042");

        let (out_kind, out_ref) = subject_parts(&subject);
        assert_eq!(out_kind, "dose-review");
        assert_eq!(out_ref, "dr-2026-042");
    }
}
