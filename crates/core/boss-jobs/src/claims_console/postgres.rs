//! Bounded row deltas under one projection-header CAS lock. This adapter never
//! loads a whole projection into memory to apply one new event batch.
use super::memory::{Row, State};
use super::*;
use boss_events::statement_budget::{StatementBudget, release_before_commit};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeSet;

const READ_ROWS: i64 = 4096;
/// Whether a cut falls inside one original transaction's half-open range,
/// asked in the form `jobs_claim_history_group_contains_cut` indexes.
/// Until backlog 803f69ec it was `lo<=$1 AND hi>$1`, which no index serves,
/// so every read scanned every retained group (95 ms of a refused read).
const SPLIT_SQL: &str = "SELECT EXISTS(SELECT 1 FROM jobs_claim_history_groups WHERE int8range(lo,hi,'[)') @> $1::bigint AND generation=$2)";
/// The head reading's rows: $1 the cut, $2 the row bound, $3 the generation.
/// Each kind is read by its own index — the claimed steps through
/// `jobs_claim_history_current_live_steps`, then their steps, station
/// claims and jobs one primary-key probe each; the live acquisitions
/// through `jobs_claim_history_current_live_acquisitions`, and those held
/// only by a live step's claim one primary-key probe each. The probes are
/// LATERAL with a LIMIT because a plain `key IN(visible)` is planned before
/// `visible` is known to be small, and the planner hashed it against every
/// retained step and job. So the read costs the live claims, not the 168 h
/// of finished steps and jobs the projection retains. Until backlog
/// 803f69ec this was one OR filter over every current row: on 2026-11-09 a
/// refused read spent 1776 ms in `rows_bound` and then 87 ms running the
/// same body warm, and 9 of 10 reads answered 503 under three busy gate
/// bays. The arms partition by kind, so UNION ALL answers the same rows the
/// OR did.
///
/// Backlog 635bbef3: the same day at 19:15Z 0 of 6 reads answered, every
/// one refused in `rows_bound` at 1930 ms with bays idle. The step arm
/// still found its rows through an index of EVERY step by status, so it
/// walked every READY step — the open queue, 1,341 across 1,378 open jobs
/// — and fetched each from the heap to learn it waited on nothing; the
/// acquisition arm read every retained acquisition the same way. The two
/// partial indexes hold exactly the rows these arms answer, and their
/// predicates are spelled here as they are in
/// `20261109192702-the-claims-head-read-indexes-only-live-claims.sql`:
/// the planner uses a partial index only when the query states its
/// predicate, so `the_head_read_reads_the_live_claims_not_the_retained_projection`
/// fails if the two drift. `IS NOT TRUE`, not `NOT`, on the held-by-claim
/// arm: an acquisition with no `active` key reads NULL there, and the OR
/// this replaced answered it.
const CURRENT_ROWS: &str = "WITH requested AS (SELECT $1::bigint), visible AS MATERIALIZED(SELECT key,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='step' AND (body->>'status'='active' OR (body->>'status'='ready' AND body->'waiting_on'<>'null'::jsonb)) LIMIT $2) SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind IN('pool','motion','unknown-resource') UNION ALL SELECT c.kind,c.key,c.seq,c.at,c.body FROM visible v CROSS JOIN LATERAL(SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind IN('step','station-claim') AND key=v.key LIMIT 2) c UNION ALL SELECT c.kind,c.key,c.seq,c.at,c.body FROM (SELECT DISTINCT body->>'job_id' AS job_id FROM visible) v CROSS JOIN LATERAL(SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='job' AND key=v.job_id LIMIT 1) c UNION ALL SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='acquisition' AND (body->'active'='true'::jsonb OR jsonb_path_exists(body, '$.stock_obligations[*] ? (@.returned_by == null)')) UNION ALL SELECT c.kind,c.key,c.seq,c.at,c.body FROM (SELECT DISTINCT body->'resource_claim'->>'id' AS id FROM visible) v CROSS JOIN LATERAL(SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='acquisition' AND key=v.id AND (body->'active'='true'::jsonb OR jsonb_path_exists(body, '$.stock_obligations[*] ? (@.returned_by == null)')) IS NOT TRUE LIMIT 1) c LIMIT $2";
/// `rows_bound`: the count and bytes a rows query would answer, read before
/// any body is fetched.
fn rows_bound_sql(rows: &str) -> String {
    format!(
        "WITH bounded AS({rows}) SELECT COUNT(*)::bigint,COALESCE(SUM(octet_length(body::text)+octet_length(kind)+octet_length(key)+128),0)::bigint FROM bounded"
    )
}
// The fold fixture's budget and stall, shared with the lease operations
// since backlog ebbf38c6; the production `STATEMENT_BUDGET` is the claims
// console's, which the source's audit-tail read spends too.
#[cfg(any(test, feature = "test-support"))]
use crate::fixture_budget::{FIXTURE_BUDGET, after_the_measured_stall};
pub struct PgClaimsHistory {
    pool: PgPool,
    /// `STATEMENT_BUDGET` in production. A test judging plan SHAPE sets its
    /// own, because a stalled gate drive is not the plan (backlog 97a5a57d).
    budget: std::time::Duration,
    /// The instant a budget is measured from: `Instant::now` in production.
    /// A test injects a clock that reports time already spent, so a slow
    /// statement is a fact it states rather than one it waits for.
    clock: fn() -> tokio::time::Instant,
}
impl PgClaimsHistory {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            budget: STATEMENT_BUDGET,
            clock: tokio::time::Instant::now,
        }
    }
    /// The 97a5a57d seam, for a fixture outside this module: an adapter on
    /// its own `budget`, measured from its own `clock`. A test of what the
    /// projection FOLDS states its stall through `clock` and outlasts a
    /// loaded gate drive through `budget`; the 2 s production claim stays
    /// pinned by `the_production_budget_refuses_a_read_whose_clock_is_spent`
    /// (backlog 21217d8a: gate d7b85890 spent it inside `rebuild_native`).
    #[cfg(feature = "test-support")]
    pub fn for_fixture(
        pool: PgPool,
        budget: std::time::Duration,
        clock: fn() -> tokio::time::Instant,
    ) -> Self {
        Self {
            pool,
            budget,
            clock,
        }
    }
    /// The adapter every fixture that judges what the projection FOLDS
    /// runs on: [`FIXTURE_BUDGET`], measured from a clock that has already
    /// lost [`crate::fixture_budget::MEASURED_STALL`], so an answer it gives is the fold's and
    /// never the gate drive's. Spelled once here because three test files
    /// each spelled it (or took production's 2 s) and the ones that took
    /// production's lost to a loaded gate drive: main gate 643e3baf at f150f78f
    /// failed claims_console_history.rs's
    /// cumulative_materialized_bytes_are_bounded_without_truncating_valid_waiting_claims
    /// on `pre-commit statement budget expired`, 5 s of io.full in 1465 s
    /// (backlog ebbf38c6). `the_claims_history_fixtures_never_take_the_production_budget`
    /// refuses a test that constructs the production budget again.
    #[cfg(feature = "test-support")]
    pub fn for_fold_fixture(pool: PgPool) -> Self {
        Self::for_fixture(pool, FIXTURE_BUDGET, after_the_measured_stall)
    }
    fn deadline(&self) -> tokio::time::Instant {
        (self.clock)() + self.budget
    }
    fn budget(&self) -> StatementBudget {
        StatementBudget::until(self.deadline())
    }
}
fn unavailable(error: impl std::fmt::Display) -> ClaimsError {
    ClaimsError::Unavailable(error.to_string())
}
fn invalid(error: impl std::fmt::Display) -> ClaimsError {
    ClaimsError::Invalid(error.to_string())
}
async fn finish<T>(
    mut tx: Transaction<'_, Postgres>,
    result: Result<T, ClaimsError>,
) -> Result<T, ClaimsError> {
    match result {
        Ok(value) => {
            if let Err(error) = release_before_commit(&mut tx).await {
                let rollback = tx.rollback().await;
                return Err(unavailable(format!(
                    "commit preparation failed: {error}; rollback acknowledgement: {rollback:?}"
                )));
            }
            tx.commit().await.map_err(|e| {
                unavailable(format!(
                    "COMMIT acknowledgement unavailable; projection effect uncertain: {e}"
                ))
            })?;
            Ok(value)
        }
        Err(original) => match tx.rollback().await {
            Ok(()) => Err(original),
            Err(error) => Err(unavailable(format!(
                "{original}; rollback acknowledgement unavailable: {error}"
            ))),
        },
    }
}

fn dependencies(batch: &HistoryBatch) -> Result<Vec<(String, String)>, ClaimsError> {
    let mut keys = BTreeSet::new();
    for fact in &batch.facts {
        if fact.source != "jobs" {
            continue;
        }
        if memory::unknown_resource(fact)? {
            keys.insert(("unknown-resource".into(), fact.kind.clone()));
        }
        let value: Value = serde_json::from_str(&fact.payload).map_err(unavailable)?;
        let mut add = |kind: &str, key: Option<&str>| {
            if let Some(key) = key {
                keys.insert((kind.into(), key.into()));
            }
        };
        match fact.kind.as_str() {
            "jobs.pool.published" | "jobs.pool.retired" => add("pool", value["name"].as_str()),
            "jobs.job.created" | "jobs.job.updated" => add("job", value["id"].as_str()),
            "jobs.step.created" | "jobs.step.updated" => add("step", value["id"].as_str()),
            crate::flow_resources::RESERVED
            | crate::flow_resources::RELEASED
            | crate::resource_leases::IDENTITY_BOUND
            | crate::resource_leases::LEASE_EXPIRED => {
                add("acquisition", value["claim"]["id"].as_str())
            }
            crate::resource_leases::STOCK_RETURNED => {
                add("acquisition", value["obligation"]["claim"]["id"].as_str())
            }
            crate::flow_resources::REFUSED | crate::events::STATION_RESOURCE_REFUSED => {
                let step = if fact.kind == crate::events::STATION_RESOURCE_REFUSED {
                    let receipt: Value = serde_json::from_str(
                        value["receipt_json"]
                            .as_str()
                            .ok_or_else(|| unavailable("station receipt missing"))?,
                    )
                    .map_err(unavailable)?;
                    receipt["refused_step"]["id"].as_str().map(str::to_owned)
                } else {
                    value["step_id"].as_str().map(str::to_owned)
                };
                if let (Some(group), Some(step)) = (&fact.original_group, step) {
                    keys.insert(("refusal".into(), format!("{}:{step}", group.reference.id)));
                }
            }
            _ => {}
        }
    }
    Ok(keys.into_iter().collect())
}
type WireRow = (String, String, i64, DateTime<Utc>, Value);
fn row((kind, key, seq, at, body): WireRow) -> Row {
    Row {
        kind,
        key,
        seq,
        at,
        body,
    }
}

impl PgClaimsHistory {
    async fn apply_batch(
        &self,
        expected: Option<ProjectionCursor>,
        batch: &HistoryBatch,
        source: Option<SourceObservation>,
    ) -> Result<(), ClaimsError> {
        // Refuse before opening a transaction or allocating unbounded SQL input.
        if batch.facts.len() > 1024
            || batch.facts.iter().map(|f| f.payload.len()).sum::<usize>() > 8 * 1024 * 1024
        {
            return Err(invalid("projection batch exceeds1024facts/8MiB"));
        }
        let keys = dependencies(batch)?;
        let budget = self.budget();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let result=async {
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_head(singleton,seq,at,observed) VALUES(true,0,$1,false) ON CONFLICT DO NOTHING")
                .bind(batch.through_at).execute(&mut *tx).await.map_err(unavailable)?;
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let (seq,at,observed,generation,source_head,source_at,source_failed):(i64,DateTime<Utc>,bool,i64,Option<i64>,Option<DateTime<Utc>>,Option<DateTime<Utc>>)=sqlx::query_as("SELECT seq,at,observed,generation,source_head,source_at,source_failed_at FROM jobs_claim_history_head WHERE singleton FOR UPDATE")
                .fetch_one(&mut *tx).await.map_err(unavailable)?;
            let cursor=ProjectionCursor{generation,through:if observed {seq}else {0}};
            if expected.is_some_and(|expected|expected!=cursor) {return Err(invalid("prepared projection cursor/generation changed"))}
            let mut state=State{generation,source_failed,source:source_head.zip(source_at).map(|(audit_head,observed_at)|SourceObservation{audit_head,observed_at}),..State::default()};if observed {state.head=Some((seq,at));}
            let (kinds,ids):(Vec<_>,Vec<_>)=keys.into_iter().unzip();
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let current:Vec<WireRow>=sqlx::query_as("SELECT c.kind,c.key,c.seq,c.at,c.body FROM jobs_claim_history_current c JOIN UNNEST($1::text[],$2::text[]) AS wanted(kind,key) ON c.kind=wanted.kind AND c.key=wanted.key WHERE c.generation=$3")
                .bind(kinds).bind(ids).bind(generation).fetch_all(&mut *tx).await.map_err(unavailable)?;
            for value in current {let value=row(value);state.current.insert((value.kind.clone(),value.key.clone()),value);}
            let sequences:Vec<_>=batch.facts.iter().map(|f|f.seq).collect();let events:Vec<_>=batch.facts.iter().map(|f|f.event_id).collect();
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let known:Vec<(i64,uuid::Uuid,String)>=sqlx::query_as("SELECT seq,event_id,fingerprint FROM jobs_claim_history_facts WHERE (seq=ANY($1) OR event_id=ANY($2)) AND generation=$3")
                .bind(sequences).bind(events).bind(generation).fetch_all(&mut *tx).await.map_err(unavailable)?;
            for (seq,id,fingerprint) in known {state.fingerprints.insert(seq,fingerprint);state.events.insert(id,seq);}
            let ids:Vec<_>=batch.facts.iter().filter_map(|f|f.original_group.as_ref().map(|g|g.reference.id)).collect();
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let groups:Vec<(uuid::Uuid,i64,i64,String)>=sqlx::query_as("SELECT id,lo,hi,evidence FROM jobs_claim_history_groups WHERE id=ANY($1) AND generation=$2")
                .bind(ids).bind(generation).fetch_all(&mut *tx).await.map_err(unavailable)?;
            for (id,lo,hi,evidence) in groups {state.groups.insert(id,(lo,hi));state.group_evidence.insert(id,evidence);}
            let reducer=InMemoryClaimsHistory::from_state(state);
            reducer.apply_batch(expected,batch,source)?;
            let state=reducer.into_state()?;
            let facts:Vec<_>=state.events.iter().map(|(id,seq)|json!({"seq":seq,"event_id":id,"fingerprint":state.fingerprints[seq],"at":state.cuts.get(seq).copied().unwrap_or(batch.through_at)})).collect();
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_facts SELECT $2,f.* FROM jsonb_to_recordset($1) AS f(seq bigint,event_id uuid,fingerprint text,at timestamptz) ON CONFLICT DO NOTHING")
                .bind(json!(facts)).bind(generation).execute(&mut *tx).await.map_err(unavailable)?;
            let cuts:Vec<_>=state.cuts.iter().map(|(seq,at)|json!({"seq":seq,"at":at})).collect();
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_cuts SELECT $2,f.* FROM jsonb_to_recordset($1) AS f(seq bigint,at timestamptz) ON CONFLICT DO NOTHING")
                .bind(json!(cuts)).bind(generation).execute(&mut *tx).await.map_err(unavailable)?;
            let groups:Vec<_>=state.groups.iter().map(|(id,(lo,hi))|json!({"id":id,"lo":lo,"hi":hi,"evidence":state.group_evidence[id]})).collect();
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_groups SELECT $2,g.* FROM jsonb_to_recordset($1) AS g(id uuid,lo bigint,hi bigint,evidence text) ON CONFLICT DO NOTHING")
                .bind(json!(groups)).bind(generation).execute(&mut *tx).await.map_err(unavailable)?;
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_rows SELECT $2,r.* FROM jsonb_to_recordset($1) AS r(kind text,key text,seq bigint,at timestamptz,body jsonb) ON CONFLICT DO NOTHING")
                .bind(json!(state.rows)).bind(generation).execute(&mut *tx).await.map_err(unavailable)?;
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_current SELECT $2,r.* FROM jsonb_to_recordset($1) AS r(kind text,key text,seq bigint,at timestamptz,body jsonb) ON CONFLICT(generation,kind,key) DO UPDATE SET seq=EXCLUDED.seq,at=EXCLUDED.at,body=EXCLUDED.body")
                .bind(json!(state.current.values().collect::<Vec<_>>())).bind(generation).execute(&mut *tx).await.map_err(unavailable)?;
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("UPDATE jobs_claim_history_head SET seq=$1,at=$2,observed=true,source_head=$3,source_at=$4,source_failed_at=$5 WHERE singleton")
                .bind(batch.through).bind(batch.through_at).bind(state.source.map(|s|s.audit_head)).bind(state.source.map(|s|s.observed_at)).bind(state.source_failed).execute(&mut *tx).await.map_err(unavailable)?;
            Ok(())
        }.await;
        finish(tx, result).await
    }
}
#[async_trait]
impl ClaimsHistory for PgClaimsHistory {
    async fn prune(
        &self,
        expected: ProjectionCursor,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<usize, ClaimsError> {
        if !(1..=1024).contains(&limit) {
            return Err(invalid("retention pass must delete1..1024rows"));
        }
        let floor = now - chrono::Duration::hours(memory::RETENTION_HOURS);
        let metrics_floor = floor - chrono::Duration::hours(crate::regions::MAX_WINDOW_HOURS);
        let budget = self.budget();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let result=async {
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let known:Option<(i64,i64,i64)>=sqlx::query_as("SELECT generation,seq,pruned_through FROM jobs_claim_history_head WHERE singleton FOR UPDATE").fetch_optional(&mut *tx).await.map_err(unavailable)?;
            let (generation,through,mut pruned)=known.ok_or_else(||unavailable("no observed projection to prune"))?;
            if expected!=(ProjectionCursor{generation,through}) {return Err(invalid("prune cursor changed"))}
            // The identifiers/predicates are private fixed source, never an API
            // selector. One shared remaining bound covers ALL six tables.
            let obsolete="((t.kind IN('step','station-claim') AND EXISTS(SELECT 1 FROM jobs_claim_history_current j WHERE j.generation=p.gen AND j.kind='job' AND j.key=t.body->>'job_id' AND j.at<p.floor AND j.body->>'status' IN('closed','cancelled'))) OR (t.kind='job' AND EXISTS(SELECT 1 FROM jobs_claim_history_current j WHERE j.generation=p.gen AND j.kind='job' AND j.key=t.key AND j.at<p.floor AND j.body->>'status' IN('closed','cancelled')) AND NOT EXISTS(SELECT 1 FROM jobs_claim_history_current s WHERE s.generation=p.gen AND s.kind IN('step','station-claim') AND s.body->>'job_id'=t.key)) OR (t.kind='acquisition' AND EXISTS(SELECT 1 FROM jobs_claim_history_current a WHERE a.generation=p.gen AND a.kind='acquisition' AND a.key=t.key AND a.at<p.floor AND a.body->'active'='false'::jsonb)))";
            let obsolete=obsolete.replace("a.body->'active'='false'::jsonb", "a.body->'active'='false'::jsonb AND NOT jsonb_path_exists(a.body, '$.stock_obligations[*] ? (@.returned_by == null)')");
            let rows=format!("(t.kind IN('release','refusal') AND t.at<p.metrics_floor) OR (t.kind NOT IN('release','refusal') AND t.at<p.floor AND ({obsolete} OR EXISTS(SELECT 1 FROM jobs_claim_history_rows newer WHERE newer.generation=p.gen AND newer.kind=t.kind AND newer.key=t.key AND newer.seq>t.seq AND newer.at<=p.floor)))");
            let current=format!("(t.kind IN('release','refusal') AND t.at<p.metrics_floor) OR (t.kind NOT IN('release','refusal') AND t.at<p.floor AND {obsolete} AND NOT EXISTS(SELECT 1 FROM jobs_claim_history_rows r WHERE r.generation=p.gen AND r.kind=t.kind AND r.key=t.key))");
            let mut total=0;
            for (table,condition,sequence) in [
                ("jobs_claim_history_facts","t.at<p.floor".to_owned(),"seq"),
                ("jobs_claim_history_cuts","t.at<p.floor".to_owned(),"seq"),
                ("jobs_claim_history_groups","t.hi<=p.pruned".to_owned(),"hi"),
                ("jobs_claim_history_rows",rows,"seq"),
                ("jobs_claim_history_current",current,"seq"),
            ] {
                if total==limit {break}
                let sql=format!("WITH params AS(SELECT $1::bigint AS gen,$2::timestamptz AS floor,$3::timestamptz AS metrics_floor,$4::bigint AS lim,$5::bigint AS pruned), selected AS(SELECT t.ctid FROM {table} t CROSS JOIN params p WHERE t.generation<>p.gen OR (t.generation=p.gen AND ({condition})) LIMIT (SELECT lim FROM params)) DELETE FROM {table} d WHERE d.ctid IN(SELECT ctid FROM selected) RETURNING generation,{sequence}");
                budget.refresh(&mut tx).await.map_err(unavailable)?;
                let deleted:Vec<(i64,i64)>=sqlx::query_as(&sql).bind(generation).bind(floor).bind(metrics_floor).bind((limit-total) as i64).bind(pruned).fetch_all(&mut *tx).await.map_err(unavailable)?;
                if table=="jobs_claim_history_cuts" {for (row_generation,seq) in &deleted {if *row_generation==generation {pruned=pruned.max(*seq);}}}
                total+=deleted.len();
            }
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("UPDATE jobs_claim_history_head SET pruned_through=$1 WHERE singleton").bind(pruned).execute(&mut *tx).await.map_err(unavailable)?;
            Ok(total)
        }.await;
        finish(tx, result).await
    }
    async fn cursor(&self) -> Result<ProjectionCursor, ClaimsError> {
        // The adapter's one budget, not a second literal 2 s: a fixture's
        // budget covers the cursor read its every pass starts with.
        let known: Option<(i64, i64)> = tokio::time::timeout_at(
            self.deadline(),
            sqlx::query_as("SELECT generation,seq FROM jobs_claim_history_head WHERE singleton")
                .fetch_optional(&self.pool),
        )
        .await
        .map_err(|_| unavailable("projection cursor deadline exhausted"))?
        .map_err(unavailable)?;
        let (generation, through) = known.unwrap_or((0, 0));
        Ok(ProjectionCursor {
            generation,
            through,
        })
    }
    async fn begin_rebuild(
        &self,
        expected: ProjectionCursor,
    ) -> Result<ProjectionCursor, ClaimsError> {
        let budget = self.budget();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let result=async {
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("INSERT INTO jobs_claim_history_head(singleton,seq,at,observed) VALUES(true,0,clock_timestamp(),false) ON CONFLICT DO NOTHING").execute(&mut *tx).await.map_err(unavailable)?;
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let (generation,through):(i64,i64)=sqlx::query_as("SELECT generation,seq FROM jobs_claim_history_head WHERE singleton FOR UPDATE").fetch_one(&mut *tx).await.map_err(unavailable)?;
            if expected!=(ProjectionCursor{generation,through}) {return Err(invalid("rebuild cursor changed"))}
            let generation=generation.checked_add(1).ok_or_else(||invalid("projection generation exhausted"))?;
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            sqlx::query("UPDATE jobs_claim_history_head SET generation=$1,seq=0,at=clock_timestamp(),observed=false,pruned_through=0,source_head=NULL,source_at=NULL,source_failed_at=NULL WHERE singleton").bind(generation).execute(&mut *tx).await.map_err(unavailable)?;
            Ok(ProjectionCursor{generation,through:0})
        }.await;
        finish(tx, result).await
    }
    async fn advance(
        &self,
        expected: ProjectionCursor,
        batch: &HistoryBatch,
        source: SourceObservation,
    ) -> Result<(), ClaimsError> {
        self.apply_batch(Some(expected), batch, Some(source)).await
    }
    async fn source_failed(
        &self,
        expected: ProjectionCursor,
        at: DateTime<Utc>,
    ) -> Result<(), ClaimsError> {
        let budget = self.budget();
        let mut tx = self.pool.begin().await.map_err(unavailable)?;
        let result=async {
            budget.refresh(&mut tx).await.map_err(unavailable)?;
            let changed=sqlx::query("UPDATE jobs_claim_history_head SET source_failed_at=$1 WHERE singleton AND generation=$2 AND seq=$3")
                .bind(at).bind(expected.generation).bind(expected.through).execute(&mut *tx).await.map_err(unavailable)?.rows_affected();
            if changed!=1 {return Err(invalid("source-failure cursor changed or is uninitialized"))}Ok(())
        }.await;
        finish(tx, result).await
    }
    /// The adapter's one budget, measured from its own clock: production's
    /// 2 s from now, and a fold fixture's 300 s from its stated stall, for
    /// the source's tail read as for every statement here (backlog ebbf38c6).
    fn source_deadline(&self) -> tokio::time::Instant {
        self.deadline()
    }
    #[cfg(feature = "test-support")]
    async fn materialize(&self, batch: &HistoryBatch) -> Result<(), ClaimsError> {
        self.apply_batch(None, batch, None).await
    }
    async fn read(
        &self,
        as_of: Option<i64>,
        hours: i64,
        now: DateTime<Utc>,
    ) -> Result<ClaimsReading, ClaimsError> {
        self.read_traced(as_of, hours, now, ReadTrace::default())
            .await
    }
    async fn read_traced(
        &self,
        as_of: Option<i64>,
        hours: i64,
        now: DateTime<Utc>,
        trace: ReadTrace,
    ) -> Result<ClaimsReading, ClaimsError> {
        if !(1..=crate::regions::MAX_WINDOW_HOURS).contains(&hours) {
            return Err(invalid("window must be1..168hours"));
        }
        let budget = self.budget();
        trace.start("begin");
        let mut tx =
            self.pool.begin().await.map_err(|e| {
                unavailable(format!("query budget/refusal: {e}; {}", trace.describe()))
            })?;
        let result=async {
            trace.finish();trace.start("snapshot");sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            trace.finish();trace.start("head");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let (head,head_at,generation,source_head,source_at,source_failed,pruned):(i64,DateTime<Utc>,i64,Option<i64>,Option<DateTime<Utc>>,Option<DateTime<Utc>>,i64)=sqlx::query_as("SELECT seq,at,generation,source_head,source_at,source_failed_at,pruned_through FROM jobs_claim_history_head WHERE singleton AND observed")
                .fetch_optional(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?.ok_or_else(||unavailable("no observed materialized horizon"))?;
            if as_of.is_none() {
                if let Some(at)=source_failed {return Err(unavailable(format!("native source unavailable at{at}; materialized through{head}")))}
                if source_head.is_some_and(|source|source>head) {return Err(unavailable(format!("catch-up pending: materialized through{head}, observed audit head{source_head:?}")))}
            }
            let cut=as_of.unwrap_or(head);if cut<0 || cut>head {return Err(invalid("requested sequence exceeds materialized horizon"))}
            if as_of.is_some() && cut<=pruned && cut!=head {return Err(ClaimsError::OutsideRetention("requested cut was pruned outside the supported window".into()))}
            trace.finish();trace.start("split");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let split:bool=sqlx::query_scalar(SPLIT_SQL)
                .bind(cut).bind(generation).fetch_one(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            if split {return Err(invalid("requested sequence bisects an original transaction"))}
            let at=if cut==head {head_at}else {
                trace.finish();trace.start("cut_at");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
                sqlx::query_scalar("SELECT at FROM jobs_claim_history_cuts WHERE seq=$1 AND generation=$2").bind(cut).bind(generation).fetch_optional(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?.ok_or_else(||invalid("requested sequence has no recorded time basis"))?
            };
            if as_of.is_some() && at<now-chrono::Duration::hours(memory::RETENTION_HOURS) {return Err(ClaimsError::OutsideRetention("requested cut is older than168hours".into()))}
            // Both branches use the same parameter shape; current reads have no
            // history scan, and historical reads reconstruct only projection rows.
            let sql=if cut==head {CURRENT_ROWS.to_owned()}else {
                let latest="SELECT DISTINCT ON(kind,key) kind,key,seq,at,body FROM jobs_claim_history_rows WHERE seq<=$1 AND generation=$3 AND kind=ANY(ARRAY['pool','motion','job','step','station-claim','acquisition','unknown-resource']) ORDER BY kind,key,seq DESC";
                format!("WITH latest AS NOT MATERIALIZED ({latest}), visible AS MATERIALIZED(SELECT * FROM latest WHERE kind='step' AND (body->>'status'='active' OR (body->>'status'='ready' AND body->'waiting_on'<>'null'::jsonb)) LIMIT $2) SELECT kind,key,seq,at,body FROM latest l WHERE kind IN('pool','motion','unknown-resource') OR (kind IN('step','station-claim') AND key IN(SELECT key FROM visible)) OR (kind='job' AND key IN(SELECT body->>'job_id' FROM visible)) OR (kind='acquisition' AND (body->'active'='true'::jsonb OR jsonb_path_exists(body, '$.stock_obligations[*] ? (@.returned_by == null)') OR key IN(SELECT body->'resource_claim'->>'id' FROM visible))) LIMIT $2")
            };
            let precheck=rows_bound_sql(&sql);
            trace.finish();trace.start("rows_bound");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let (count,bytes):(i64,i64)=sqlx::query_as(&precheck).bind(cut).bind(READ_ROWS+1).bind(generation).fetch_one(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            if count>READ_ROWS || bytes>2*MAX_READING_BYTES as i64 {return Err(unavailable("materialized row/byte bound exceeded before body fetch"))}
            trace.finish();trace.start("rows");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let found:Vec<WireRow>=sqlx::query_as(&sql).bind(cut).bind(READ_ROWS+1).bind(generation).fetch_all(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            if found.len() as i64>READ_ROWS {return Err(unavailable("claims projection read exceeds4096row bound"))}
            let mut rows:Vec<_>=found.into_iter().map(row).collect();
            trace.finish();trace.start("metrics_plan");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let metrics_sql="WITH selected AS (SELECT DISTINCT ON(kind,key) kind,key,body FROM jobs_claim_history_rows WHERE kind=ANY(ARRAY['release','refusal']) AND seq<=$1 AND at>=$2 AND generation=$4 ORDER BY kind,key,seq DESC), metrics AS (SELECT body->'reason'->>'pool' AS pool,0::bigint AS releases,1::bigint AS refusals FROM selected WHERE kind='refusal' UNION ALL SELECT a->>'pool',1::bigint,0::bigint FROM selected CROSS JOIN LATERAL jsonb_array_elements(body->'allocations') AS a WHERE kind='release') SELECT pool,SUM(releases)::bigint AS releases,SUM(refusals)::bigint AS refusals FROM metrics GROUP BY pool LIMIT $3";
            let precheck=format!("WITH bounded AS({metrics_sql}) SELECT COUNT(*)::bigint,COALESCE(SUM(octet_length(pool)+64),0)::bigint FROM bounded");
            trace.finish();trace.start("metrics_bound");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let (count,bytes):(i64,i64)=sqlx::query_as(&precheck).bind(cut).bind(at-chrono::Duration::hours(hours)).bind(READ_ROWS+1).bind(generation).fetch_one(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            if count>READ_ROWS || bytes>2*MAX_READING_BYTES as i64 {return Err(unavailable("materialized metrics row/byte bound exceeded before fetch"))}
            trace.finish();trace.start("metrics");budget.refresh(&mut tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            let metrics:Vec<(String,i64,i64)>=sqlx::query_as(metrics_sql)
                .bind(cut).bind(at-chrono::Duration::hours(hours)).bind(READ_ROWS+1).bind(generation).fetch_all(&mut *tx).await.map_err(|e|unavailable(format!("query budget/refusal: {e}; {}",trace.describe())))?;
            if metrics.len() as i64>READ_ROWS {return Err(unavailable("pool metrics exceed4096row bound"))}
            for (key,releases,refusals) in metrics {rows.push(Row{kind:"metrics".into(),key,seq:cut,at,body:json!({"releases":releases,"refusals":refusals})});}
            trace.finish();
            check_rows(rows.iter())?;
            memory::reading(rows,cut,at,hours,now,projection_basis(ProjectionCursor{generation,through:head},source_head.zip(source_at).map(|(audit_head,observed_at)|SourceObservation{audit_head,observed_at}),source_failed))
        }.await;
        finish(tx, result).await
    }
}

#[cfg(test)]
mod read_plan_tests {
    use super::*;

    /// The production latency claim, deterministically: a reader on the
    /// unchanged production budget whose clock reports the measured stall
    /// already spent refuses at its first budgeted statement, by name. The
    /// control is the same reader on the fixture budget, which gets past
    /// that statement and answers the empty fixture's own refusal instead.
    #[tokio::test]
    async fn the_production_budget_refuses_a_read_whose_clock_is_spent() {
        assert_eq!(STATEMENT_BUDGET, std::time::Duration::from_secs(2));
        let db = boss_testing::TestDb::new().await;
        let now = Utc::now();
        let production = PgClaimsHistory {
            clock: after_the_measured_stall,
            ..PgClaimsHistory::new(db.pool.clone())
        };
        let refused = production.read(None, 24, now).await;
        match refused {
            Err(ClaimsError::Unavailable(why)) => assert!(
                why.contains("pre-commit statement budget expired")
                    && why.contains("active_query=head"),
                "{why}"
            ),
            other => panic!("the production budget must refuse a spent clock: {other:?}"),
        }
        let control = PgClaimsHistory {
            budget: FIXTURE_BUDGET,
            clock: after_the_measured_stall,
            ..PgClaimsHistory::new(db.pool.clone())
        };
        match control.read(None, 24, now).await {
            Err(ClaimsError::Unavailable(why)) => assert!(
                why.contains("no observed materialized horizon") && !why.contains("budget"),
                "{why}"
            ),
            other => panic!("the fixture budget reaches the head query: {other:?}"),
        }
    }

    #[tokio::test]
    async fn range_index_preserves_the_original_half_open_transaction_boundary() {
        let db = boss_testing::TestDb::new().await;
        for (lo, hi, cut, expected) in [
            (7_i64, 8_i64, 6_i64, false),
            (7, 8, 7, true),
            (7, 8, 8, false),
            (7, 8, 9, false),
            (7, 7, 7, false),
            (i64::MAX - 1, i64::MAX, i64::MAX - 1, true),
            (i64::MAX - 1, i64::MAX, i64::MAX, false),
        ] {
            let (old, new): (bool, bool) = sqlx::query_as(
                "SELECT ($1::bigint<=$3 AND $2::bigint>$3),int8range($1,$2,'[)') @> $3::bigint",
            )
            .bind(lo)
            .bind(hi)
            .bind(cut)
            .fetch_one(&db.pool)
            .await
            .unwrap();
            assert_eq!(
                (old, new),
                (expected, expected),
                "lo={lo} hi={hi} cut={cut}"
            );
        }
    }

    /// Every node of one `EXPLAIN (FORMAT JSON)` plan, depth first.
    fn plan_nodes(node: &serde_json::Value) -> Vec<&serde_json::Value> {
        std::iter::once(node)
            .chain(
                node["Plans"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(plan_nodes),
            )
            .collect()
    }

    /// Rows every relation scan returned or discarded, over all its loops.
    /// EXPLAIN reports both counts per loop, so each is multiplied back.
    fn rows_scanned(plan: &serde_json::Value) -> f64 {
        plan_nodes(&plan[0]["Plan"])
            .into_iter()
            .filter(|node| node["Relation Name"].is_string())
            .map(|node| {
                let per_loop = [
                    "Actual Rows",
                    "Rows Removed by Filter",
                    "Rows Removed by Index Recheck",
                ]
                .iter()
                .map(|key| node[*key].as_f64().unwrap_or(0.0))
                .sum::<f64>();
                per_loop * node["Actual Loops"].as_f64().unwrap_or(1.0)
            })
            .sum()
    }

    /// The head read as main ran it until backlog 803f69ec: one OR filter
    /// over every current row of the generation. Kept only as this test's
    /// negative control — the fixture must be large enough to show it.
    const HEAD_ROWS_UNTIL_803F69EC: &str = "WITH requested AS (SELECT $1::bigint), latest AS NOT MATERIALIZED (SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3), visible AS MATERIALIZED(SELECT * FROM latest WHERE kind='step' AND (body->>'status'='active' OR (body->>'status'='ready' AND body->'waiting_on'<>'null'::jsonb)) LIMIT $2) SELECT kind,key,seq,at,body FROM latest l WHERE kind IN('pool','motion','unknown-resource') OR (kind IN('step','station-claim') AND key IN(SELECT key FROM visible)) OR (kind='job' AND key IN(SELECT body->>'job_id' FROM visible)) OR (kind='acquisition' AND (body->'active'='true'::jsonb OR jsonb_path_exists(body, '$.stock_obligations[*] ? (@.returned_by == null)') OR key IN(SELECT body->'resource_claim'->>'id' FROM visible))) LIMIT $2";
    /// The head read as 803f69ec left it, until backlog 635bbef3: its step
    /// arm walked the status index over every READY step and fetched each
    /// to learn it waited on nothing, and its acquisition arm read every
    /// retained acquisition to test it. Kept only as a negative control.
    const HEAD_ROWS_UNTIL_635BBEF3: &str = "WITH requested AS (SELECT $1::bigint), visible AS MATERIALIZED(SELECT key,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='step' AND body->>'status' IN('active','ready') AND (body->>'status'='active' OR body->'waiting_on'<>'null'::jsonb) LIMIT $2) SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind IN('pool','motion','unknown-resource') UNION ALL SELECT c.kind,c.key,c.seq,c.at,c.body FROM visible v CROSS JOIN LATERAL(SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind IN('step','station-claim') AND key=v.key LIMIT 2) c UNION ALL SELECT c.kind,c.key,c.seq,c.at,c.body FROM (SELECT DISTINCT body->>'job_id' AS job_id FROM visible) v CROSS JOIN LATERAL(SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='job' AND key=v.job_id LIMIT 1) c UNION ALL SELECT kind,key,seq,at,body FROM jobs_claim_history_current WHERE generation=$3 AND kind='acquisition' AND (body->'active'='true'::jsonb OR jsonb_path_exists(body, '$.stock_obligations[*] ? (@.returned_by == null)') OR key IN(SELECT body->'resource_claim'->>'id' FROM visible)) LIMIT $2";
    const SPLIT_UNTIL_803F69EC: &str = "SELECT EXISTS(SELECT 1 FROM jobs_claim_history_groups WHERE lo<=$1 AND hi>$1 AND generation=$2)";

    /// Backlog 803f69ec: on 2026-11-09 GET /api/claims answered 503 on 9 of
    /// 10 reads under three busy gate bays, and a refused read measured
    /// `split` 95 ms and `rows_bound` 1776 ms, then `rows` — the same
    /// query body, now cached — 87 ms. The head read scanned every step
    /// and job the projection retains (168 h of them) to find the few that
    /// are claimed, so its cost was the retained table's, paid from disk
    /// whenever the bays had evicted it; `split` scanned every original
    /// transaction group past the range index built for exactly that
    /// question. What this pins is the shape, never a time (97a5a57d):
    /// under both the custom and the generic plan a prepared statement
    /// settles on, `rows_bound` and `split` read rows in proportion to the
    /// live claims, not to the retained ones — and the shapes they replace,
    /// on the same fixture, read every retained row.
    #[tokio::test]
    async fn the_head_read_reads_the_live_claims_not_the_retained_projection() {
        let db = boss_testing::TestDb::new().await;
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL statement_timeout='30s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        // Retained and finished: 40,000 completed steps of 10,000 closed
        // jobs, and 20,000 single-event transaction groups. Unclaimed but
        // open (backlog 635bbef3): the queue — 1,400 ready steps of 1,400
        // open jobs waiting on nothing, where the live system held 1,341
        // ready steps across 1,378 open jobs on 2026-11-09 19:20Z — and
        // 1,000 released acquisitions whose stock came back. Live: two
        // active steps of one open job, one ready step waiting on a pool,
        // one pool, one active acquisition, one released acquisition still
        // owing stock, and one held by a live step's claim alone.
        let retained = 52_400.0;
        sqlx::query("INSERT INTO jobs_claim_history_current(generation,kind,key,seq,at,body) SELECT 0,'step','done-'||n,n,now(),jsonb_build_object('status','completed','job_id','closed-'||(n/4)) FROM generate_series(1,40000) n").execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs_claim_history_current(generation,kind,key,seq,at,body) SELECT 0,'job','closed-'||n,n,now(),jsonb_build_object('status','closed') FROM generate_series(0,9999) n").execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs_claim_history_current(generation,kind,key,seq,at,body) SELECT 0,'step','queued-'||n,50000+n,now(),jsonb_build_object('status','ready','job_id','queue-'||n,'waiting_on',NULL,'resource_claim',NULL) FROM generate_series(1,1400) n").execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs_claim_history_current(generation,kind,key,seq,at,body) SELECT 0,'job','queue-'||n,52000+n,now(),jsonb_build_object('status','open') FROM generate_series(1,1400) n").execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs_claim_history_current(generation,kind,key,seq,at,body) SELECT 0,'acquisition','released-'||n,54000+n,now(),jsonb_build_object('active',false,'stock_obligations',jsonb_build_array(jsonb_build_object('returned_by','step-'||n))) FROM generate_series(1,1000) n").execute(&mut *tx).await.unwrap();
        sqlx::query(r#"INSERT INTO jobs_claim_history_current(generation,kind,key,seq,at,body) VALUES
            (0,'step','live-1',40001,now(),'{"status":"active","job_id":"open-1","resource_claim":{"id":"acq-1"}}'),
            (0,'step','live-2',40002,now(),'{"status":"active","job_id":"open-1","resource_claim":{"id":"acq-held"}}'),
            (0,'step','waiting-1',40003,now(),'{"status":"ready","job_id":"open-1","waiting_on":{"pool":"gate-bays"}}'),
            (0,'job','open-1',40004,now(),'{"status":"open"}'),
            (0,'pool','gate-bays',40005,now(),'{"name":"gate-bays"}'),
            (0,'acquisition','acq-1',40006,now(),'{"active":true}'),
            (0,'acquisition','acq-stock',40007,now(),'{"active":false,"stock_obligations":[{"returned_by":null}]}'),
            (0,'acquisition','acq-held',40008,now(),'{"stock_obligations":[]}')"#).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs_claim_history_groups(generation,id,lo,hi,evidence) SELECT 0,md5(n::text)::uuid,n,n+1,'retained original' FROM generate_series(1,40000,2) n").execute(&mut *tx).await.unwrap();
        for table in ["jobs_claim_history_current", "jobs_claim_history_groups"] {
            sqlx::query(&format!("ANALYZE {table}"))
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let head_rows = rows_bound_sql(CURRENT_ROWS);
        let control_rows = rows_bound_sql(HEAD_ROWS_UNTIL_803F69EC);
        let queue_rows = rows_bound_sql(HEAD_ROWS_UNTIL_635BBEF3);
        let rows_args = format!("(0,{},0)", READ_ROWS + 1);
        // The first two are the reads; the rest are the negative controls.
        for (name, sql, types, args, live) in [
            (
                "rows_bound",
                head_rows.as_str(),
                "(bigint,bigint,bigint)",
                rows_args.as_str(),
                true,
            ),
            ("split", SPLIT_SQL, "(bigint,bigint)", "(40001,0)", true),
            (
                "rows_bound_until_803f69ec",
                control_rows.as_str(),
                "(bigint,bigint,bigint)",
                rows_args.as_str(),
                false,
            ),
            (
                "split_until_803f69ec",
                SPLIT_UNTIL_803F69EC,
                "(bigint,bigint)",
                "(40001,0)",
                false,
            ),
            (
                "rows_bound_until_635bbef3",
                queue_rows.as_str(),
                "(bigint,bigint,bigint)",
                rows_args.as_str(),
                false,
            ),
        ] {
            sqlx::query(&format!("PREPARE probe{types} AS {sql}"))
                .execute(&mut *tx)
                .await
                .unwrap();
            for mode in ["auto", "force_custom_plan", "force_generic_plan"] {
                sqlx::query(&format!("SET LOCAL plan_cache_mode={mode}"))
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                let plan: serde_json::Value = sqlx::query_scalar(&format!(
                    "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) EXECUTE probe{args}"
                ))
                .fetch_one(&mut *tx)
                .await
                .unwrap();
                let read = rows_scanned(&plan);
                println!("HEAD_PLAN {name} mode={mode} rows_scanned={read} {plan}");
                // Every live row, looked up a few times over, is under 100;
                // one pass over the retained rows is 50,000 or 20,000, and
                // one over the queue and the released acquisitions 2,400.
                assert_eq!(
                    read < 100.0,
                    live,
                    "{name} under {mode} read {read} rows of {retained} retained: {plan}"
                );
            }
            sqlx::query("DEALLOCATE probe")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        // The rows the head read answers are the live ones, exactly.
        let mut answered: Vec<(String, String)> =
            sqlx::query_as(&format!("SELECT kind,key FROM ({CURRENT_ROWS}) r"))
                .bind(0_i64)
                .bind(READ_ROWS + 1)
                .bind(0_i64)
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        answered.sort();
        let mut before: Vec<(String, String)> = sqlx::query_as(&format!(
            "SELECT kind,key FROM ({HEAD_ROWS_UNTIL_803F69EC}) r"
        ))
        .bind(0_i64)
        .bind(READ_ROWS + 1)
        .bind(0_i64)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        before.sort();
        assert_eq!(answered, before);
        let mut queue: Vec<(String, String)> = sqlx::query_as(&format!(
            "SELECT kind,key FROM ({HEAD_ROWS_UNTIL_635BBEF3}) r"
        ))
        .bind(0_i64)
        .bind(READ_ROWS + 1)
        .bind(0_i64)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        queue.sort();
        assert_eq!(answered, queue);
        assert_eq!(answered.len(), 8, "{answered:?}");
        tx.rollback().await.unwrap();
    }
}
