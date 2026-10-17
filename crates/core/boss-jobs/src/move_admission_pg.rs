//! Infrastructure for one repeatable-read audit snapshot and its separate projection.
use crate::{
    move_admission::{self, Observation, Snapshot},
    move_diagnosis::{Diagnosis, HistoricalEvent},
};
use boss_core::event::Event;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

fn historical(r: boss_events::tail_http::HistoricalEntry) -> HistoricalEvent {
    HistoricalEvent {
        seq: r.seq,
        created_at: Some(r.created_at),
        event: Event {
            id: r.event_id,
            timestamp: r.timestamp,
            source: r.source,
            kind: r.kind,
            payload: r.payload,
        },
    }
}
pub(crate) async fn snapshot(pool: &PgPool, d: &Diagnosis) -> Result<Snapshot, String> {
    let mut tx = begin_snapshot(pool).await?;
    let result = snapshot_in(&mut tx, d).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(result)
}
async fn begin_snapshot(pool: &PgPool) -> Result<Transaction<'_, Postgres>, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(tx)
}
async fn snapshot_in(
    tx: &mut Transaction<'_, Postgres>,
    d: &Diagnosis,
) -> Result<Snapshot, String> {
    let (run, parent) = move_admission::identities(d)?;
    let (captured_max_seq, acquired_at): (i64, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT COALESCE(MAX(id),0),clock_timestamp() FROM audit_log")
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    let cutoff = d.original.r#move.cause_seq;
    let mut slices = Vec::new();
    let mut payload_bytes = 0_i64;
    for id in [run, parent] {
        let (count,bytes):(i64,i64)=sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(octet_length(payload::text)),0)::bigint FROM audit_log WHERE id<=$2 AND id<=$3 AND (payload->>'job_id'=$1 OR payload->>'id'=$1)")
            .bind(id.to_string()).bind(cutoff).bind(captured_max_seq).fetch_one(&mut **tx).await.map_err(|e|e.to_string())?;
        payload_bytes = payload_bytes.saturating_add(bytes);
        if count > move_admission::MAX_ROWS as i64
            || payload_bytes > move_admission::MAX_BYTES as i64
        {
            return Err("native admission snapshot row or byte bound exceeded".into());
        }
        let rows=sqlx::query_as::<_,boss_events::tail_http::HistoricalEntry>("SELECT id AS seq,event_id,timestamp,created_at,source,kind,payload FROM audit_log WHERE id<=$2 AND id<=$3 AND (payload->>'job_id'=$1 OR payload->>'id'=$1) ORDER BY id LIMIT 4097")
            .bind(id.to_string()).bind(cutoff).bind(captured_max_seq).fetch_all(&mut **tx).await.map_err(|e|e.to_string())?;
        if rows.len() > move_admission::MAX_ROWS {
            return Err("historical event bound exceeded".into());
        }
        slices.push(rows.into_iter().map(historical).collect());
    }
    let admissions=sqlx::query_as::<_,boss_events::tail_http::HistoricalEntry>("SELECT id AS seq,event_id,timestamp,created_at,source,kind,payload FROM audit_log WHERE id<=$2 AND source='jobs' AND kind='jobs.job.created' AND (payload->>'id'=$1 OR payload->>'job_id'=$1) ORDER BY id LIMIT 2")
        .bind(run.to_string()).bind(captured_max_seq).fetch_all(&mut **tx).await.map_err(|e|e.to_string())?;
    let mut slices = slices.into_iter();
    Ok(Snapshot {
        captured_max_seq,
        acquired_at,
        run: slices.next().ok_or("run snapshot missing")?,
        parent: slices.next().ok_or("parent snapshot missing")?,
        native_admissions: admissions.into_iter().map(historical).collect(),
    })
}
pub(crate) async fn get(pool: &PgPool, id: Uuid) -> Result<Option<Observation>, String> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT body FROM yard_move_admission_observations WHERE id=$1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
    row.map(|(body,)| serde_json::from_str(&body).map_err(|e| e.to_string()))
        .transpose()
}
pub(crate) async fn append(pool: &PgPool, o: &Observation, e: &Event) -> Result<bool, String> {
    move_admission::validate_event(o, e)?;
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("LOCK TABLE yard_move_admission_observations IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    let original_row = sqlx::query("SELECT * FROM yard_moves WHERE seq=$1 FOR SHARE")
        .bind(o.diagnosis.original.seq)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("original missing")?;
    let original = crate::moves::pg::recorded(&original_row).map_err(|e| e.to_string())?;
    let diagnosis: Option<(String,)> =
        sqlx::query_as("SELECT body FROM yard_move_corrections WHERE id=$1 FOR SHARE")
            .bind(o.diagnosis.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    let entry = serde_json::from_str::<crate::move_diagnosis::Entry>(
        &diagnosis.ok_or("observation diagnosis missing")?.0,
    )
    .map_err(|e| e.to_string())?;
    move_admission::validate_binding(o, &original, &[entry])?;
    let spent: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM yard_move_corrections WHERE id=$1)")
            .bind(o.id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    if spent {
        return Err("observation request already names a correction ledger entry".into());
    }
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT body FROM yard_move_admission_observations WHERE id=$1")
            .bind(o.id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    if let Some((body,)) = existing {
        let old: Observation = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        if !move_admission::same_request(&old, o) {
            return Err("observation request identity conflict".into());
        }
        return Ok(false);
    }
    sqlx::query("INSERT INTO yard_move_admission_observations(id,original_seq,diagnosis_id,body,event_id) VALUES($1,$2,$3,$4,$5)")
        .bind(o.id).bind(o.diagnosis.original.seq).bind(o.diagnosis.id).bind(serde_json::to_string(o).map_err(|e|e.to_string())?).bind(e.id).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    boss_events::outbox::record_event_in_tx(&mut tx, e).await?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::move_diagnosis::{Inputs, Status, hash};
    use crate::moves::{Move, Recorded};
    use serde_json::json;

    #[tokio::test]
    async fn native_writers_serialize_and_a_read_snapshot_stays_fixed_across_commit() {
        let db = boss_testing::TestDb::new().await;
        let native: serde_json::Value =
            serde_json::from_str(include_str!("moves/dispatch/native-registered-review.json"))
                .unwrap();
        let run: boss_core::job::Job = serde_json::from_value(native["run"].clone()).unwrap();
        let parent: boss_core::job::Job = serde_json::from_value(native["parent"].clone()).unwrap();
        let at = boss_clock_client::wall_now();
        let cause = Event::new(
            "jobs",
            "step.assigned.task",
            json!({"job_id":parent.id,"_partition":"real"}),
            at,
        );
        let (cause_seq,cause_at):(i64,chrono::DateTime<chrono::Utc>)=sqlx::query_as("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,$2,$3,$4,$5) RETURNING id,created_at")
            .bind(cause.id).bind(cause.timestamp).bind(&cause.source).bind(&cause.kind).bind(&cause.payload).fetch_one(&db.pool).await.unwrap();
        let original = Recorded {
            seq: 1,
            r#move: Move {
                at: cause_at,
                packet: run.id.to_string(),
                kind: "agent-run".into(),
                label: "snapshot control".into(),
                from: Some("shed".into()),
                to: Some("shop-floor".into()),
                declared: Some(false),
                cause_event_id: cause.id,
                cause_seq,
                cause_kind: cause.kind.clone(),
                handoff_from: Some(parent.id.to_string()),
                lineage: Some("packet".into()),
                aboard: vec![],
                terminal: None,
                dispatch: None,
            },
            mover_version: None,
        };
        let d = Diagnosis {
            id: Uuid::new_v4(),
            original_hash: hash(&original).unwrap(),
            original,
            model: crate::move_diagnosis::model(),
            inputs: Inputs {
                run: vec![],
                parent: vec![HistoricalEvent {
                    seq: cause_seq,
                    created_at: Some(cause_at),
                    event: cause,
                }],
                workflows: vec![],
                stations: vec![],
                station_history: vec![],
            },
            status: Status::Unknown,
            why: "admission missing".into(),
            dispatch: None,
        };
        let mut pending = db.pool.begin().await.unwrap();
        let admission = Event::new(
            "jobs",
            crate::events::JOB_CREATED,
            json!(run),
            at - chrono::Duration::hours(1),
        );
        let lower:i64=sqlx::query_scalar("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,$2,$3,$4,$5) RETURNING id")
            .bind(admission.id).bind(admission.timestamp).bind(&admission.source).bind(&admission.kind).bind(&admission.payload).fetch_one(&mut *pending).await.unwrap();
        // The real audit trigger allocates ids AFTER its transaction chain lock.
        // A higher committed insertion cannot pass the pending admission.
        let mut second = db.pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout='100ms'")
            .execute(&mut *second)
            .await
            .unwrap();
        let refused=sqlx::query("INSERT INTO audit_log(event_id,timestamp,source,kind,payload) VALUES($1,$2,'jobs','snapshot.control','{}')")
            .bind(Uuid::new_v4()).bind(at).execute(&mut *second).await.unwrap_err();
        assert_eq!(
            refused
                .as_database_error()
                .and_then(|e| e.code())
                .as_deref(),
            Some("55P03")
        );
        second.rollback().await.unwrap();
        assert!(lower > cause_seq);
        let mut tx = begin_snapshot(&db.pool).await.unwrap();
        // Establish the SAME transaction snapshot before the lower insertion commits.
        let captured: i64 = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(captured, cause_seq);
        pending.commit().await.unwrap();
        let frozen = snapshot_in(&mut tx, &d).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(frozen.captured_max_seq, captured);
        assert!(
            frozen.native_admissions.is_empty(),
            "repeatable read must retain the original observation snapshot across commit"
        );
        let later = snapshot(&db.pool, &d).await.unwrap();
        assert_eq!(later.captured_max_seq, lower);
        assert_eq!(later.native_admissions.len(), 1);
        assert_eq!(later.native_admissions[0].seq, lower);
        assert_eq!(later.native_admissions[0].event.id, admission.id);
    }
}
