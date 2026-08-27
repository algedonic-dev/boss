//! Postgres `ViewsRepo`.

use async_trait::async_trait;
use sqlx::{PgPool, Row, postgres::PgRow};

use crate::error::ViewsError;
use crate::filter;
use crate::port::ViewsRepo;
use crate::types::{View, ViewInput, ViewLayout, ViewSource, Visibility};

pub struct PgViewsRepo {
    pool: PgPool,
}

impl PgViewsRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const COLS: &str = "id, owner_id, title, source, filter, columns, layout, visibility, \
                    created_at, updated_at";

fn storage(e: sqlx::Error) -> ViewsError {
    ViewsError::Storage(e.to_string())
}

fn row_to_view(r: &PgRow) -> Result<View, ViewsError> {
    let source_raw: String = r.try_get("source").map_err(storage)?;
    let layout_raw: String = r.try_get("layout").map_err(storage)?;
    let vis_raw: String = r.try_get("visibility").map_err(storage)?;
    // A stored value the enums don't know is a schema/code mismatch,
    // not a row to guess at. Say which value and which column.
    let source = ViewSource::parse(&source_raw)
        .ok_or_else(|| ViewsError::Storage(format!("unknown view source {source_raw:?}")))?;
    let layout = ViewLayout::parse(&layout_raw)
        .ok_or_else(|| ViewsError::Storage(format!("unknown view layout {layout_raw:?}")))?;
    let visibility = Visibility::parse(&vis_raw)
        .ok_or_else(|| ViewsError::Storage(format!("unknown visibility {vis_raw:?}")))?;
    Ok(View {
        id: r.try_get("id").map_err(storage)?,
        owner_id: r.try_get("owner_id").map_err(storage)?,
        title: r.try_get("title").map_err(storage)?,
        source,
        filter: r.try_get("filter").map_err(storage)?,
        columns: r.try_get("columns").map_err(storage)?,
        layout,
        visibility,
        created_at: r.try_get("created_at").map_err(storage)?,
        updated_at: r.try_get("updated_at").map_err(storage)?,
    })
}

#[async_trait]
impl ViewsRepo for PgViewsRepo {
    async fn list_for_viewer(&self, viewer_id: &str) -> Result<Vec<View>, ViewsError> {
        let sql = format!(
            "SELECT {COLS} FROM views \
             WHERE owner_id = $1 OR visibility = 'shared' \
             ORDER BY updated_at DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(viewer_id)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
        rows.iter().map(row_to_view).collect()
    }

    async fn get_for_viewer(&self, id: &str, viewer_id: &str) -> Result<View, ViewsError> {
        // Ownership is in the WHERE clause, not a post-fetch check: a
        // row the caller may not see never leaves the database, and
        // the miss is indistinguishable from a bad id.
        let sql = format!(
            "SELECT {COLS} FROM views \
             WHERE id = $1 AND (owner_id = $2 OR visibility = 'shared')"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(viewer_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))?;
        row_to_view(&row)
    }

    async fn create(&self, owner_id: &str, input: &ViewInput) -> Result<View, ViewsError> {
        // Reject a malformed filter before it reaches storage, so it
        // fails for its author rather than for whoever opens it later.
        filter::compile(&input.filter)?;
        let sql = format!(
            "INSERT INTO views (id, owner_id, title, source, filter, columns, layout, visibility) \
             VALUES (gen_random_uuid()::text, $1, $2, $3, $4, $5, $6, $7) RETURNING {COLS}"
        );
        let row = sqlx::query(&sql)
            .bind(owner_id)
            .bind(&input.title)
            .bind(input.source.as_str())
            .bind(&input.filter)
            .bind(input.columns.as_slice())
            .bind(input.layout.as_str())
            .bind(input.visibility.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
        row_to_view(&row)
    }

    async fn replace(
        &self,
        id: &str,
        owner_id: &str,
        input: &ViewInput,
    ) -> Result<View, ViewsError> {
        filter::compile(&input.filter)?;
        // `owner_id` is a WHERE term, never a SET term: it scopes the
        // update to a View this caller owns and cannot transfer
        // ownership. Shared means readable, not writable.
        let sql = format!(
            "UPDATE views SET title = $3, source = $4, filter = $5, \
                    columns = $6, layout = $7, visibility = $8, updated_at = NOW() \
             WHERE id = $1 AND owner_id = $2 RETURNING {COLS}"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .bind(owner_id)
            .bind(&input.title)
            .bind(input.source.as_str())
            .bind(&input.filter)
            .bind(input.columns.as_slice())
            .bind(input.layout.as_str())
            .bind(input.visibility.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .ok_or_else(|| ViewsError::NotFound(id.to_string()))?;
        row_to_view(&row)
    }

    async fn delete(&self, id: &str, owner_id: &str) -> Result<(), ViewsError> {
        let res = sqlx::query("DELETE FROM views WHERE id = $1 AND owner_id = $2")
            .bind(id)
            .bind(owner_id)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        if res.rows_affected() == 0 {
            return Err(ViewsError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::os_map::OsMapRepo for PgViewsRepo {
    async fn os_map(&self, limit: i64) -> Result<crate::os_map::OsMap, ViewsError> {
        use crate::os_map::{OsMap, OsMapEdge, classify, nodes_from_edges};

        // One pass: take the most recent `limit` step completions,
        // pair each with the previous completion on the SAME Job
        // (that pairing IS the handoff), resolve both actors to a
        // department, and aggregate.
        //
        // `lag` partitions by job_id so a handoff never spans two
        // Jobs. Automation collapses to one `dispatcher` node rather
        // than one node per rule — `/it/dispatcher` is the drill-down
        // for what is inside it.
        let rows: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "WITH recent AS (
                 SELECT audit_id,
                        payload->>'job_id'   AS job_id,
                        payload->>'_actor'   AS actor
                 FROM event_facts
                 WHERE kind = 'jobs.step.completed'
                   AND payload->>'job_id' IS NOT NULL
                 ORDER BY audit_id DESC
                 LIMIT $1
             ),
             paired AS (
                 -- Sim-ness comes from the JOB, not the event. A Job is
                 -- simulated or real from creation and immutably so, so a
                 -- real operator clicking a simulated Job does not make
                 -- that handoff real. Reading the event's own marker had
                 -- the map disagreeing with the epoch trim about the same
                 -- traffic — two surfaces answering one question two ways.
                 SELECT r.actor,
                        COALESCE(j.simulated, false) AS simulated,
                        LAG(r.actor) OVER (PARTITION BY r.job_id ORDER BY r.audit_id)
                            AS prev_actor
                 FROM recent r
                 LEFT JOIN jobs j ON j.id::text = r.job_id
             ),
             resolved AS (
                 -- Actor → node, mirroring `ActorId::from_str`'s branch
                 -- order: `automation:` first (its slug may itself carry
                 -- colons, e.g. `automation:rule:bill-approve`), then any
                 -- remaining colon-bearing id, which is an agent session
                 -- (`<mode>:<model>`). No employee id carries a colon, so
                 -- an agent can never be mistaken for staff — nor left in
                 -- `unresolved`, where it used to land.
                 SELECT
                     COALESCE(ep.department,
                              CASE WHEN p.prev_actor LIKE 'automation:%'
                                   THEN 'dispatcher'
                                   WHEN p.prev_actor LIKE '%:%'
                                   THEN 'agent' ELSE 'unresolved' END) AS src,
                     COALESCE(ea.department,
                              CASE WHEN p.actor LIKE 'automation:%'
                                   THEN 'dispatcher'
                                   WHEN p.actor LIKE '%:%'
                                   THEN 'agent' ELSE 'unresolved' END) AS dst,
                     p.simulated
                 FROM paired p
                 LEFT JOIN employees ep ON ep.id = p.prev_actor
                 LEFT JOIN employees ea ON ea.id = p.actor
                 WHERE p.prev_actor IS NOT NULL
             )
             SELECT src, dst,
                    COUNT(*)::bigint AS handoffs,
                    COUNT(*) FILTER (WHERE simulated)::bigint AS simulated
             FROM resolved
             GROUP BY src, dst
             ORDER BY handoffs DESC",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let edges: Vec<OsMapEdge> = rows
            .into_iter()
            .map(|(source, target, handoffs, simulated)| OsMapEdge {
                source,
                target,
                handoffs,
                simulated,
            })
            .collect();

        // Labels come from the Class registry, which already owns the
        // tenant's department vocabulary — `it` is "IT" and `qa` is
        // "QA" there. Humanising the code here instead produced "It"
        // and "Qa": a second, worse copy of a fact the registry
        // already holds (CLAUDE.md §9a). `classify` stays as the
        // fallback for the reserved ids and for a department with no
        // Class row.
        let labels: Vec<(String, String)> = sqlx::query_as(
            "SELECT code, display_name FROM classes
             WHERE subject_kind = 'employee' AND member_attribute = 'department'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;
        let labels: std::collections::HashMap<String, String> = labels.into_iter().collect();

        let handoffs_considered = edges.iter().map(|e| e.handoffs).sum();
        let high_water: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(audit_id), 0) FROM event_facts")
                .fetch_one(&self.pool)
                .await
                .map_err(|e| ViewsError::Storage(e.to_string()))?;

        Ok(OsMap {
            nodes: nodes_from_edges(&edges, |id| match labels.get(id) {
                Some(display) => (display.clone(), crate::os_map::NodeKind::Department),
                None => classify(id),
            }),
            edges,
            handoffs_considered,
            high_water,
        })
    }
}

#[async_trait::async_trait]
impl crate::flow::FlowRepo for PgViewsRepo {
    async fn flow(
        &self,
        owner_roles: &[String],
        limit: i64,
    ) -> Result<crate::flow::Flow, ViewsError> {
        use crate::flow::{Flow, FlowJob, FlowStep};

        // Wall clock, deliberately: `created_at` rather than
        // `timestamp`. See the module docs — `timestamp` is the
        // authoritative clock, which on a demo deployment is the
        // simulator's, so it cannot answer "how long did a person
        // wait". This is the only view that reads `audit_log` instead
        // of `event_facts`, because `event_facts` keeps only
        // `occurred_at` and drops the wall clock entirely.
        //
        // Which kinds count comes from the registry: a Workflow naming
        // one of `owner_roles` as its owner is this team's work. No
        // list of kinds lives in code (CLAUDE.md §9), and a Job with
        // no declared owner never enters — which is what keeps 85
        // mis-marked restock Jobs out of the IT team's numbers.
        let job_rows: Vec<(
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "WITH team_kinds AS (
                     SELECT DISTINCT k.kind, k.metadata->>'owner_role' AS owner_role
                     FROM workflows k
                     WHERE k.metadata->>'owner_role' = ANY($1)
                 ),
                 filed AS (
                     SELECT a.payload->>'id' AS job_id, min(a.created_at) AS filed_at
                     FROM audit_log a
                     WHERE a.kind = 'jobs.job.created'
                     GROUP BY 1
                 ),
                 activity AS (
                     SELECT coalesce(a.payload->>'job_id', a.payload->>'id') AS job_id,
                            max(a.created_at) AS last_at
                     FROM audit_log a
                     GROUP BY 1
                 )
                 SELECT j.id::text, j.kind, j.title, j.status,
                        to_char(f.filed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                        to_char(ac.last_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
                 FROM jobs j
                 JOIN team_kinds tk ON tk.kind = j.kind
                 LEFT JOIN filed f ON f.job_id = j.id::text
                 LEFT JOIN activity ac ON ac.job_id = j.id::text
                 WHERE NOT j.simulated
                 ORDER BY f.filed_at DESC NULLS LAST
                 LIMIT $2",
        )
        .bind(owner_roles)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let job_ids: Vec<String> = job_rows.iter().map(|r| r.0.clone()).collect();

        // Steps for those Jobs, each with the wall-clock time of its
        // newest event. Raw on purpose: which step carries the
        // decision is a registry question the client already answers
        // once (apps/web/src/jobs/fork.ts), and a second copy of that
        // rule has drifted before.
        let step_rows: Vec<(
            String,
            String,
            String,
            serde_json::Value,
            serde_json::Value,
            Option<String>,
        )> = sqlx::query_as(
            "WITH touched AS (
                     SELECT a.payload->>'step_id' AS step_id, max(a.created_at) AS last_at
                     FROM audit_log a
                     WHERE a.payload ? 'step_id'
                     GROUP BY 1
                 )
                 SELECT s.job_id::text, s.id::text, s.status, s.metadata, s.fields,
                        to_char(t.last_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
                 FROM steps s
                 LEFT JOIN touched t ON t.step_id = s.id::text
                 WHERE s.job_id::text = ANY($1)
                 ORDER BY s.job_id, s.sort_order",
        )
        .bind(&job_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let mut kinds: Vec<String> = job_rows.iter().map(|r| r.1.clone()).collect();
        kinds.sort();
        kinds.dedup();

        let jobs = job_rows
            .into_iter()
            .map(
                |(job_id, kind, title, status, filed_at, last_activity_at)| {
                    let steps = step_rows
                        .iter()
                        .filter(|s| s.0 == job_id)
                        .map(
                            |(_, step_id, st, metadata, fields, last_written_at)| FlowStep {
                                step_id: step_id.clone(),
                                status: st.clone(),
                                metadata: metadata.clone(),
                                // `fields` is the declared schema; the client
                                // finds the fork by the field it bears.
                                field_names: fields
                                    .as_array()
                                    .map(|a| {
                                        a.iter()
                                            .filter_map(|f| {
                                                f.get("name")
                                                    .and_then(|n| n.as_str())
                                                    .map(String::from)
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                                last_written_at: last_written_at.clone(),
                            },
                        )
                        .collect();
                    FlowJob {
                        job_id,
                        // Every Job here came through the `team_kinds`
                        // join, so its kind declares an owner_role; the
                        // caller's list is the set that matched.
                        owner_role: owner_roles.first().cloned().unwrap_or_default(),
                        kind,
                        title,
                        status,
                        filed_at,
                        last_activity_at,
                        steps,
                    }
                },
            )
            .collect();

        // `as_of` comes from the DATABASE clock, not the process's.
        //
        // Not a lint dodge — `Utc::now()` here would be a second clock
        // in a calculation that already has one. Every timestamp this
        // view returns is `audit_log.created_at`, written by Postgres
        // `now()`, and the page subtracts `as_of` from them to render
        // "open for 4h". Reading the reference point from anywhere
        // else means the answer drifts by whatever the two clocks
        // disagree about, silently and only under skew.
        let as_of: String = sqlx::query_scalar(
            "SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        Ok(Flow {
            owner_roles: owner_roles.to_vec(),
            kinds,
            jobs,
            as_of,
        })
    }
}

#[async_trait]
impl crate::fleet::FleetRepo for PgViewsRepo {
    async fn fleet(&self, workflow_kind: &str) -> Result<crate::fleet::Fleet, ViewsError> {
        use crate::fleet::{Fleet, FleetNode};
        use std::collections::BTreeMap;

        let open_jobs: i64 =
            sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = $1 AND status = 'open'")
                .bind(workflow_kind)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| ViewsError::Storage(e.to_string()))?;

        // Depth per step of the Workflow's shape. The group key falls
        // back to the title for slug-less steps (pre-migration-100
        // rows, slug-less Workflow specs) so they pile up visibly
        // instead of vanishing — the client buckets unmatched keys
        // off-map. Only the live set enters: in-flight steps of open
        // Jobs, which is what keeps this O(work-in-flight) and lets
        // the partial indexes (steps_assignee, steps_authority_role)
        // cover their branches.
        let depth_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
            "SELECT COALESCE(NULLIF(s.spec_slug, ''), s.title) AS slug,
                    count(*) FILTER (WHERE s.status = 'ready'),
                    count(*) FILTER (WHERE s.status = 'active'),
                    count(*) FILTER (WHERE s.assignee_id IS NULL OR s.assignee_id = '')
             FROM steps s
             JOIN jobs j ON j.id = s.job_id
             WHERE j.kind = $1 AND j.status = 'open'
               AND s.status IN ('ready', 'active')
             GROUP BY 1",
        )
        .bind(workflow_kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let role_rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT COALESCE(NULLIF(s.spec_slug, ''), s.title) AS slug,
                    s.metadata->>'authority_role',
                    count(*)
             FROM steps s
             JOIN jobs j ON j.id = s.job_id
             WHERE j.kind = $1 AND j.status = 'open'
               AND s.status IN ('ready', 'active')
               AND s.metadata->>'authority_role' IS NOT NULL
             GROUP BY 1, 2",
        )
        .bind(workflow_kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        // Wall clock, deliberately — `created_at`, never `timestamp`
        // (which is sim-authoritative on a demo deployment; see
        // flow.rs, the same doctrine). The still-ready steps are
        // gathered first so the audit scan is bounded by the live
        // set, not the log.
        let oldest_rows: Vec<(String, Option<String>)> = sqlx::query_as(
            "WITH live AS (
                     SELECT s.id, COALESCE(NULLIF(s.spec_slug, ''), s.title) AS slug
                     FROM steps s
                     JOIN jobs j ON j.id = s.job_id
                     WHERE j.kind = $1 AND j.status = 'open' AND s.status = 'ready'
                 )
                 SELECT live.slug,
                        to_char(min(a.created_at) AT TIME ZONE 'UTC',
                                'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
                 FROM live
                 JOIN audit_log a ON a.payload->>'step_id' = live.id::text
                                 AND a.kind LIKE 'step.ready.%'
                 GROUP BY 1",
        )
        .bind(workflow_kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let mut by_role: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for (slug, role, n) in role_rows {
            by_role.entry(slug).or_default().insert(role, n);
        }
        let oldest: BTreeMap<String, Option<String>> = oldest_rows.into_iter().collect();

        let nodes = depth_rows
            .into_iter()
            .map(|(slug, ready, active, unassigned)| FleetNode {
                by_role: by_role.remove(&slug).unwrap_or_default(),
                oldest_ready_wall: oldest.get(&slug).cloned().flatten(),
                slug,
                ready,
                active,
                unassigned,
            })
            .collect();

        let as_of: String = sqlx::query_scalar(
            "SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        Ok(Fleet {
            workflow_kind: workflow_kind.to_string(),
            open_jobs,
            nodes,
            as_of,
        })
    }
}

#[async_trait]
impl crate::stages::StageDurationsRepo for PgViewsRepo {
    async fn stage_durations(
        &self,
        workflow_kind: &str,
        window_days: i64,
    ) -> Result<crate::stages::StageDurations, ViewsError> {
        use crate::stages::{Stage, StageDurations};

        // Wall clock throughout (`created_at`), never the
        // sim-authoritative `timestamp` — see flow.rs for the doctrine
        // and the incident behind it. Duration = first done minus
        // first ready per step; steps with no done yet have an age
        // (fleet's oldest-wait), not a duration, and stay out.
        let rows: Vec<(String, i64, f64, f64, f64)> = sqlx::query_as(
            "WITH ready AS (
                     SELECT a.payload->>'step_id' AS sid, min(a.created_at) AS t
                     FROM audit_log a
                     WHERE a.kind LIKE 'step.ready.%'
                     GROUP BY 1
                 ),
                 done AS (
                     SELECT a.payload->>'step_id' AS sid, min(a.created_at) AS t
                     FROM audit_log a
                     WHERE a.kind LIKE 'step.done.%'
                       AND a.created_at > now() - make_interval(days => $2::int)
                     GROUP BY 1
                 ),
                 hops AS (
                     SELECT COALESCE(NULLIF(s.spec_slug, ''), s.title) AS slug,
                            EXTRACT(EPOCH FROM (done.t - ready.t)) AS secs
                     FROM steps s
                     JOIN jobs j ON j.id = s.job_id
                     JOIN done ON done.sid = s.id::text
                     JOIN ready ON ready.sid = s.id::text
                     WHERE j.kind = $1 AND done.t >= ready.t
                 )
                 SELECT slug,
                        count(*)::bigint,
                        percentile_cont(0.5) WITHIN GROUP (ORDER BY secs)::float8,
                        percentile_cont(0.9) WITHIN GROUP (ORDER BY secs)::float8,
                        max(secs)::float8
                 FROM hops
                 GROUP BY slug
                 ORDER BY slug",
        )
        .bind(workflow_kind)
        .bind(window_days)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let stages = rows
            .into_iter()
            .map(|(slug, completed, p50, p90, max)| Stage {
                slug,
                completed,
                p50_seconds: p50,
                p90_seconds: p90,
                max_seconds: max,
            })
            .collect();

        let as_of: String = sqlx::query_scalar(
            "SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        Ok(StageDurations {
            workflow_kind: workflow_kind.to_string(),
            window_days,
            stages,
            as_of,
        })
    }

    async fn stage_runs(
        &self,
        workflow_kind: &str,
        limit: i64,
    ) -> Result<crate::stages::StageRuns, ViewsError> {
        use crate::stages::{RunStage, StageRun, StageRuns};

        // Same doctrine as the aggregate above: wall clock
        // (`created_at`), duration = first done minus first ready per
        // step, a ready-only step is a wait (None), not a duration.
        // `jobs.created_at` orders recency — NOT the sim-calendar
        // `opened_on`.
        let rows: Vec<(uuid::Uuid, String, String, String, String, Option<f64>)> = sqlx::query_as(
            "WITH recent AS (
                     SELECT id, title, status, created_at
                     FROM jobs
                     WHERE kind = $1
                     ORDER BY created_at DESC
                     LIMIT $2
                 ),
                 ready AS (
                     SELECT a.payload->>'step_id' AS sid, min(a.created_at) AS t
                     FROM audit_log a
                     WHERE a.kind LIKE 'step.ready.%'
                     GROUP BY 1
                 ),
                 done AS (
                     SELECT a.payload->>'step_id' AS sid, min(a.created_at) AS t
                     FROM audit_log a
                     WHERE a.kind LIKE 'step.done.%'
                     GROUP BY 1
                 )
                 SELECT r.id,
                        r.title,
                        r.status,
                        to_char(r.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                        COALESCE(NULLIF(s.spec_slug, ''), s.title) AS slug,
                        CASE WHEN done.t >= ready.t
                             THEN EXTRACT(EPOCH FROM (done.t - ready.t))::float8
                        END AS secs
                 FROM recent r
                 JOIN steps s ON s.job_id = r.id
                 LEFT JOIN ready ON ready.sid = s.id::text
                 LEFT JOIN done ON done.sid = s.id::text
                 ORDER BY r.created_at DESC, r.id, s.sort_order",
        )
        .bind(workflow_kind)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        let mut runs: Vec<StageRun> = Vec::new();
        for (id, title, status, created_at, slug, secs) in rows {
            let job_id = id.to_string();
            if runs.last().map(|r| r.job_id != job_id).unwrap_or(true) {
                runs.push(StageRun {
                    job_id,
                    title,
                    created_at,
                    status,
                    stages: Vec::new(),
                });
            }
            runs.last_mut().expect("just pushed").stages.push(RunStage {
                slug,
                seconds: secs,
            });
        }

        let as_of: String = sqlx::query_scalar(
            "SELECT to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ViewsError::Storage(e.to_string()))?;

        Ok(StageRuns {
            workflow_kind: workflow_kind.to_string(),
            limit,
            runs,
            as_of,
        })
    }
}
