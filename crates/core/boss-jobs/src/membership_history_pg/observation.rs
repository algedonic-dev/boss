//! Fixed native query phases, sharing the production cut and seed SQL.
use super::*;
use crate::membership_observation::{Observation, Phase};
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
        let binds = serde_json::to_vec(&(
            carried,
            cut.seq,
            SNAPSHOT_KINDS,
            excluded,
            budget.rows + 1,
            metadata.kinds,
            metadata.keys,
            budget.bytes,
        ))
        .map_err(|why| why.to_string())?;
        let result = async {
            refresh(conn, statement).await?;
            let query = sqlx::query_scalar::<_, serde_json::Value>(&sql)
                .bind(carried)
                .bind(cut.seq)
                .bind(SNAPSHOT_KINDS)
                .bind(excluded);
            let plan = if name == "full_seed" {
                query
                    .bind(budget.rows + 1)
                    .bind(metadata.kinds)
                    .bind(metadata.keys)
                    .bind(budget.bytes)
                    .fetch_one(&mut *conn)
                    .await
            } else {
                query.fetch_one(&mut *conn).await
            }
            .map_err(|why| why.to_string())?;
            Ok::<_, String>(plan)
        }
        .await;
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
            Err(why) => {
                phase.why = Some(why.clone());
                report.phases.push(phase);
                return Err(format!("membership observation {name}: {why}"));
            }
        }
    }
    Ok(())
}
