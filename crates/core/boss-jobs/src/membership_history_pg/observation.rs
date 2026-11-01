//! Fixed native query phases, sharing the production cut and seed SQL.
use super::*;
use crate::membership_observation::{Observation, Phase, SeedTimeout};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub async fn observe(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Observation, String> {
    observe_bounded(
        pool,
        since,
        actor,
        scope,
        limit,
        crate::membership_projection_service::FLOW_READ_DEADLINE,
        cancelled,
    )
    .await
}

pub(super) async fn observe_bounded(
    pool: &PgPool,
    since: DateTime<Utc>,
    actor: Option<String>,
    scope: &JobScope,
    limit: i64,
    bound: Duration,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Observation, String> {
    if *scope == JobScope::None {
        return Err("membership observation scope reads no packets".into());
    }
    let started = Instant::now();
    let budget =
        crate::membership_replay::ReplayBudget::new(started + bound, cancelled, usize::MAX);
    let mut report = Observation {
        state: "unavailable".into(),
        since: Some(since),
        ..Observation::default()
    };
    // The canonical owner keeps SQL and rollback through their actual
    // terminal. A late teardown is recorded, never detached as a timeout.
    let result = projection_source_observed(
        pool,
        since,
        actor,
        scope,
        None,
        limit,
        Some(&budget),
        Some(&mut report),
    )
    .await;
    report.phases.push(timed("owned_total", started, None));
    match result {
        Ok(_) => report.state = "observed".into(),
        Err(why) => report.why = Some(why),
    }
    if serde_json::to_vec(&report)
        .map_err(|why| why.to_string())?
        .len()
        > crate::membership_observation::MAX_REPORT_BYTES
    {
        return Err("membership observation exceeds native report byte bound; no complete observation available".into());
    }
    Ok(report)
}

pub(super) fn timed(name: &str, started: Instant, rows: Option<u64>) -> Phase {
    Phase {
        name: name.into(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        sql_sha256: None,
        binds_sha256: None,
        plan: None,
        rows,
        why: None,
    }
}

pub(super) async fn window_cut_order(
    conn: &mut PgConnection,
    since: DateTime<Utc>,
    cut: i64,
    statement: Option<StatementControl<'_>>,
    report: &mut Observation,
) -> Result<bool, String> {
    let binds = serde_json::to_vec(&(since, cut)).map_err(|why| why.to_string())?;
    let binds_sha = hex::encode(Sha256::digest(&binds));
    // This added statement only plans; the original boolean check below
    // remains the sole execution of the ordering predicate.
    let sql = format!("EXPLAIN (COSTS TRUE,FORMAT JSON) {WINDOW_CUT_ORDER_SQL}");
    let started = Instant::now();
    let result = async {
        refresh(conn, statement).await?;
        sqlx::query_scalar::<_, serde_json::Value>(&sql)
            .bind(since)
            .bind(cut)
            .fetch_one(&mut *conn)
            .await
            .map_err(|why| why.to_string())
    }
    .await;
    let mut phase = timed("window_cut_order_plan", started, None);
    phase.sql_sha256 = Some(hex::encode(Sha256::digest(sql.as_bytes())));
    phase.binds_sha256 = Some(binds_sha.clone());
    match result {
        Ok(plan) => {
            phase.plan = Some(plan);
            phase.why = Some(
                "planner estimates only; not the historical executor plan or runtime buffers"
                    .into(),
            );
            report.phases.push(phase);
        }
        Err(why) => {
            phase.why = Some(why.clone());
            report.phases.push(phase);
            return Err(format!(
                "membership observation window_cut_order_plan: {why}"
            ));
        }
    }
    let started = Instant::now();
    let result = async {
        refresh(conn, statement).await?;
        sqlx::query_scalar::<_, bool>(WINDOW_CUT_ORDER_SQL)
            .bind(since)
            .bind(cut)
            .fetch_one(&mut *conn)
            .await
            .map_err(|why| why.to_string())
    }
    .await;
    let mut phase = timed("window_cut_order_check", started, None);
    phase.sql_sha256 = Some(hex::encode(Sha256::digest(WINDOW_CUT_ORDER_SQL.as_bytes())));
    phase.binds_sha256 = Some(binds_sha);
    match result {
        Ok(disordered) => {
            if disordered {
                phase.why = Some(
                    "audit insertion instants do not follow audit order at the window cut".into(),
                );
            }
            report.phases.push(phase);
            Ok(disordered)
        }
        Err(why) => {
            phase.why = Some(why.clone());
            report.phases.push(phase);
            Err(format!(
                "membership observation window_cut_order_check: {why}"
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn plans(
    conn: &mut PgConnection,
    carried: &[String],
    excluded: &[String],
    cut: &Cut,
    budget: &Budget,
    metadata: MetadataKeys<'_>,
    statement: Option<StatementControl<'_>>,
    report: &mut Observation,
) -> Result<(), String> {
    for (name, suffix) in [
        ("packet_rows", " SELECT COUNT(*) FROM packet_rows"),
        ("manifest_claims", " SELECT COUNT(*) FROM manifest_claims"),
        (
            "selection_evidence",
            " SELECT COUNT(*) FROM audit_log WHERE kind='jobs.station.selection_evidence' AND id<=$2 AND payload->>'claim_event_id' IN(SELECT event_id::text FROM claims)",
        ),
        ("full_seed", ""),
    ] {
        let started = Instant::now();
        let sql = format!(
            "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON,TIMING FALSE) {}{}",
            if name == "full_seed" {
                SEED_SQL
            } else {
                SEED_CTES
            },
            suffix,
        );
        let binds = if name == "full_seed" {
            serde_json::to_vec(&(
                carried,
                cut.seq,
                SNAPSHOT_KINDS,
                excluded,
                budget.rows + 1,
                metadata.kinds,
                metadata.keys,
                budget.bytes,
            ))
        } else {
            serde_json::to_vec(&(carried, cut.seq, SNAPSHOT_KINDS, excluded))
        }
        .map_err(|why| why.to_string())?;
        let result = {
            let query = sqlx::query_scalar::<_, serde_json::Value>(&sql)
                .bind(carried)
                .bind(cut.seq)
                .bind(SNAPSHOT_KINDS)
                .bind(excluded);
            let query = if name == "full_seed" {
                query
                    .bind(budget.rows + 1)
                    .bind(metadata.kinds)
                    .bind(metadata.keys)
                    .bind(budget.bytes)
            } else {
                query
            };
            bounded_query(conn, query, statement).await
        };
        let mut phase = timed(name, started, None);
        phase.sql_sha256 = Some(hex::encode(Sha256::digest(sql.as_bytes())));
        phase.binds_sha256 = Some(hex::encode(Sha256::digest(&binds)));
        match result {
            Ok(plan) => {
                phase.rows = plan
                    .get(0)
                    .and_then(|v| v.get("Plan"))
                    .and_then(|v| v.get("Actual Rows"))
                    .and_then(serde_json::Value::as_u64);
                phase.plan = Some(plan);
                report.phases.push(phase);
                // Preserve the completed raw plan even if the original
                // total budget expires immediately after its statement.
                refresh(conn, statement).await?;
            }
            Err(failure) => {
                phase.why = Some(failure.why.clone());
                report.phases.push(phase);
                if failure.sqlstate.as_deref() == Some("57014")
                    && failure
                        .why
                        .contains("canceling statement due to statement timeout")
                    && failure.recovered
                {
                    timeout_evidence(
                        conn, name, &sql, &binds, carried, excluded, cut, budget, metadata,
                        statement, report,
                    )
                    .await;
                }
                return Err(format!("membership observation {name}: {}", failure.why));
            }
        }
    }
    Ok(())
}

struct QueryFailure {
    why: String,
    sqlstate: Option<String>,
    recovered: bool,
}

/// A timed-out PostgreSQL statement aborts its transaction. Recover only
/// this diagnostic statement, inside the same owned repeatable-read cut.
/// Successful savepoints are bounded by the four plans and three fallback
/// reads, and end with the owner's original commit or rollback.
async fn bounded_query(
    conn: &mut PgConnection,
    query: sqlx::query::QueryScalar<
        '_,
        sqlx::Postgres,
        serde_json::Value,
        sqlx::postgres::PgArguments,
    >,
    statement: Option<StatementControl<'_>>,
) -> Result<serde_json::Value, QueryFailure> {
    refresh(conn, statement).await.map_err(|why| QueryFailure {
        why,
        sqlstate: None,
        recovered: false,
    })?;
    sqlx::query("SAVEPOINT membership_observation_query")
        .execute(&mut *conn)
        .await
        .map_err(|why| QueryFailure {
            why: why.to_string(),
            sqlstate: None,
            recovered: false,
        })?;
    match query.fetch_one(&mut *conn).await {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut failure = QueryFailure {
                why: error.to_string(),
                sqlstate: error
                    .as_database_error()
                    .and_then(|db| db.code())
                    .map(|code| code.into_owned()),
                recovered: false,
            };
            // No refresh can run in an aborted transaction. Await the
            // actual recovery before any new statement-budget check.
            match sqlx::query("ROLLBACK TO SAVEPOINT membership_observation_query")
                .execute(&mut *conn)
                .await
            {
                Ok(_) => failure.recovered = true,
                Err(why) => failure
                    .why
                    .push_str(&format!("; diagnostic savepoint recovery failed: {why}")),
            }
            Err(failure)
        }
    }
}

// Same pre-DISTINCT candidate predicate as SEED_CTES, pinned below. These
// are historical rows the seed must consider, not its retained entity count.
pub(super) const PACKET_HISTORY_COUNTS_SQL: &str = "SELECT COALESCE(jsonb_agg(jsonb_build_object(\
    'packet_id',pid,'historical_rows',n) ORDER BY n DESC,pid),'[]'::jsonb) FROM (\
    SELECT original_packet_identity(a.kind,a.payload) AS pid,COUNT(*)::bigint AS n \
    FROM audit_log a WHERE original_packet_identity(a.kind,a.payload)=ANY($1) AND a.id<=$2 \
    AND a.kind=ANY($3) AND (a.original_group IS NULL OR NOT (a.original_group->>'id'=ANY($4)))\
    GROUP BY pid ORDER BY n DESC,pid LIMIT 20) counts";

#[allow(clippy::too_many_arguments)]
async fn timeout_evidence(
    conn: &mut PgConnection,
    name: &str,
    analyzed_sql: &str,
    binds: &[u8],
    carried: &[String],
    excluded: &[String],
    cut: &Cut,
    budget: &Budget,
    metadata: MetadataKeys<'_>,
    statement: Option<StatementControl<'_>>,
    report: &mut Observation,
) {
    let mut evidence = SeedTimeout {
        phase: name.into(),
        sqlstate: "57014".into(),
        ..SeedTimeout::default()
    };
    let sql = analyzed_sql.replacen(
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON,TIMING FALSE)",
        "EXPLAIN (COSTS TRUE,FORMAT JSON)",
        1,
    );
    let started = Instant::now();
    let query = sqlx::query_scalar::<_, serde_json::Value>(&sql)
        .bind(carried)
        .bind(cut.seq)
        .bind(SNAPSHOT_KINDS)
        .bind(excluded);
    let query = if name == "full_seed" {
        query
            .bind(budget.rows + 1)
            .bind(metadata.kinds)
            .bind(metadata.keys)
            .bind(budget.bytes)
    } else {
        query
    };
    let mut phase = timed(&format!("{name}_timeout_plan"), started, None);
    phase.sql_sha256 = Some(hex::encode(Sha256::digest(sql.as_bytes())));
    phase.binds_sha256 = Some(hex::encode(Sha256::digest(binds)));
    match bounded_query(conn, query, statement).await {
        Ok(plan) => {
            phase.plan = Some(plan);
            phase.why = Some("planner estimates only after the original ANALYZE timeout; not a successful seed or historical executor plan".into());
        }
        Err(failure) => phase.why = Some(failure.why),
    }
    report.phases.push(phase);

    let sql = "SELECT COALESCE((SELECT to_jsonb(i.indisvalid) FROM pg_index i WHERE i.indexrelid=to_regclass('audit_log_created_at_id') AND i.indrelid='audit_log'::regclass),'null'::jsonb)";
    let started = Instant::now();
    let result = bounded_query(
        conn,
        sqlx::query_scalar::<_, serde_json::Value>(sql),
        statement,
    )
    .await;
    let mut phase = timed("seed_timeout_order_index_valid", started, None);
    phase.sql_sha256 = Some(hex::encode(Sha256::digest(sql.as_bytes())));
    match result {
        Ok(value) => {
            evidence.order_index_valid = value.as_bool();
            if value.is_null() {
                phase.why =
                    Some("audit_log_created_at_id is absent at the original snapshot".into());
            }
        }
        Err(failure) => phase.why = Some(failure.why),
    }
    report.phases.push(phase);

    let started = Instant::now();
    let result = bounded_query(
        conn,
        sqlx::query_scalar::<_, serde_json::Value>(PACKET_HISTORY_COUNTS_SQL)
            .bind(carried)
            .bind(cut.seq)
            .bind(SNAPSHOT_KINDS)
            .bind(excluded),
        statement,
    )
    .await;
    let mut phase = timed("seed_timeout_largest_packets", started, None);
    phase.sql_sha256 = Some(hex::encode(Sha256::digest(
        PACKET_HISTORY_COUNTS_SQL.as_bytes(),
    )));
    phase.binds_sha256 = serde_json::to_vec(&(carried, cut.seq, SNAPSHOT_KINDS, excluded))
        .ok()
        .map(|bytes| hex::encode(Sha256::digest(bytes)));
    match result {
        Ok(value) => match serde_json::from_value(value) {
            Ok(counts) => {
                let counts: Vec<crate::membership_observation::PacketHistoryCount> = counts;
                phase.rows = Some(counts.len() as u64);
                evidence.largest_packets = Some(counts);
            }
            Err(why) => phase.why = Some(format!("invalid native packet-count evidence: {why}")),
        },
        Err(failure) => phase.why = Some(failure.why),
    }
    report.phases.push(phase);
    report.seed_timeout = Some(evidence);
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_testing::TestDb;

    #[tokio::test]
    async fn seed_timeout_fallback_spends_the_original_deadline_and_cancellation() {
        let db = TestDb::new().await;
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("CREATE TEMP SEQUENCE forbidden_fallback_calls")
            .execute(&mut *tx)
            .await
            .unwrap();
        for cancelled in [false, true] {
            let check = || {
                if cancelled {
                    Err("original owner cancelled".into())
                } else {
                    Ok(())
                }
            };
            let budget = StatementBudget::until(tokio::time::Instant::now());
            let failure = bounded_query(
                &mut tx,
                sqlx::query_scalar("SELECT to_jsonb(nextval('forbidden_fallback_calls'))"),
                Some(budget.checked(&check)),
            )
            .await
            .err()
            .unwrap();
            assert!(
                failure
                    .why
                    .contains(if cancelled { "cancelled" } else { "expired" })
            );
            assert!(failure.sqlstate.is_none());
        }
        let called: bool = sqlx::query_scalar("SELECT is_called FROM forbidden_fallback_calls")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert!(
            !called,
            "a deadline or cancellation must not dispatch fallback SQL"
        );
        let budget = StatementBudget::until(tokio::time::Instant::now() + Duration::from_secs(2));
        let check = || Ok(());
        let failure = bounded_query(
            &mut tx,
            sqlx::query_scalar("SELECT to_jsonb(1/0)"),
            Some(budget.checked(&check)),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(failure.sqlstate.as_deref(), Some("22012"));
        assert!(failure.recovered);
        assert!(failure.why.contains("division by zero"));
        let usable: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(usable, 1);
        tx.rollback().await.unwrap();
    }
}
