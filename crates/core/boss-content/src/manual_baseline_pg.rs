//! Raw inventory and baseline admission share the protected content boundary.
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgConnection;

use crate::manual_baseline::{
    BaselineAdmission, ManualBaseline, ManualBaselineRepository, ManualInventory, ManualSequence,
};
use crate::{ContentError, PgContent};

fn storage(e: impl ToString) -> ContentError {
    ContentError::Storage(e.to_string())
}

pub(crate) async fn sequence(conn: &mut PgConnection) -> Result<ManualSequence, String> {
    // pg_get_serial_sequence quotes the catalog-owned identifier; no caller
    // names a relation, and the returned name is also bound for setval.
    let name: String =
        sqlx::query_scalar("SELECT pg_get_serial_sequence('manual_section_history','id')")
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    let (last_value, is_called): (i64, bool) =
        sqlx::query_as(&format!("SELECT last_value,is_called FROM {name}"))
            .fetch_one(conn)
            .await
            .map_err(|e| e.to_string())?;
    Ok(ManualSequence {
        last_value,
        is_called,
    })
}

async fn inventory(conn: &mut PgConnection) -> Result<ManualInventory, ContentError> {
    let sections: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(s) FROM manual_sections s ORDER BY id")
            .fetch_all(&mut *conn)
            .await
            .map_err(storage)?;
    let history: Vec<Value> =
        sqlx::query_scalar("SELECT to_jsonb(h) FROM manual_section_history h ORDER BY id")
            .fetch_all(&mut *conn)
            .await
            .map_err(storage)?;
    let sequence = sequence(conn).await.map_err(storage)?;
    ManualInventory::from_rows(sections, history, sequence).map_err(ContentError::Validation)
}

async fn lock(conn: &mut PgConnection) -> Result<(), ContentError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(crate::rebuild::REBUILD_LOCK_KEY)
        .execute(&mut *conn)
        .await
        .map_err(storage)?;
    boss_events::outbox::lock_and_assert_log_complete(
        conn,
        &["manual_sections", "manual_section_history"],
    )
    .await
    .map_err(storage)
}

#[async_trait]
impl ManualBaselineRepository for PgContent {
    async fn manual_inventory(&self) -> Result<ManualInventory, ContentError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        lock(&mut tx).await?;
        let result = inventory(&mut tx).await?;
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }

    async fn observe_manual_baseline(
        &self,
        admission: &BaselineAdmission,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<ManualBaseline, ContentError> {
        if admission.admission_id.is_nil() {
            return Err(ContentError::Validation(
                "manual baseline requires an admission identity".into(),
            ));
        }
        // The producer owns the fact UUID. A caller's retry key may already
        // name an unrelated fact; it must never replace this stamped identity.
        let mut event = stamp.event(crate::events::BASELINE_OBSERVED, serde_json::json!({}));
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(crate::rebuild::REBUILD_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        // A retry reads the committed observation even while its outbox fact
        // awaits the relay. It never stamps a fresh time or another actor.
        let prior: Vec<(uuid::Uuid, chrono::DateTime<chrono::Utc>, String, String, Value)> = sqlx::query_as(
            "WITH witnesses AS (SELECT event_id FROM audit_log WHERE kind=$1 UNION SELECT event_id FROM event_outbox WHERE kind=$1) \
             SELECT event_id,timestamp,source,kind,payload FROM audit_log WHERE event_id IN (SELECT event_id FROM witnesses) \
             UNION ALL SELECT event_id,timestamp,source,kind,payload FROM event_outbox WHERE event_id IN (SELECT event_id FROM witnesses)")
            .bind(crate::events::BASELINE_OBSERVED).fetch_all(&mut *tx).await.map_err(storage)?;
        if !prior.is_empty() {
            let mut observed: Option<ManualBaseline> = None;
            let mut original = None;
            for (id, timestamp, source, kind, payload) in prior {
                if kind != crate::events::BASELINE_OBSERVED {
                    return Err(ContentError::Validation(
                        "manual baseline witness names a conflicting durable fact".into(),
                    ));
                }
                let baseline: ManualBaseline =
                    serde_json::from_value(payload.clone()).map_err(storage)?;
                baseline
                    .validate_fact(id, &source, timestamp, &payload)
                    .map_err(ContentError::Validation)?;
                let fact = (id, timestamp, source, kind, payload);
                if baseline.admission_id != admission.admission_id
                    || baseline.inventory.sha256 != admission.expected_sha256
                    || baseline.observed_by != stamp.actor().to_string()
                    || baseline.event_source != event.source
                    || original.as_ref().is_some_and(|old| old != &fact)
                    || observed.as_ref().is_some_and(|old| old != &baseline)
                {
                    return Err(ContentError::Validation(
                        "manual baseline already admitted or retry disagrees with its record"
                            .into(),
                    ));
                }
                original = Some(fact);
                observed = Some(baseline);
            }
            return observed.ok_or_else(|| storage("manual baseline copies could not be read"));
        }
        boss_events::outbox::lock_and_assert_log_complete(
            &mut tx,
            &["manual_sections", "manual_section_history"],
        )
        .await
        .map_err(storage)?;
        let inventory = inventory(&mut tx).await?;
        if inventory.sha256 != admission.expected_sha256 {
            return Err(ContentError::Validation(
                "manual inventory changed since it was read; baseline refused".into(),
            ));
        }
        let log_cutoff: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id),0) FROM audit_log")
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
        let baseline = ManualBaseline {
            admission_id: admission.admission_id,
            event_id: event.id,
            event_source: event.source.clone(),
            observed_at: stamp.timestamp,
            observed_by: stamp.actor().to_string(),
            inventory,
            log_cutoff,
        };
        let fields = serde_json::to_value(&baseline).map_err(storage)?;
        let fields = fields
            .as_object()
            .ok_or_else(|| storage("baseline fields are not an object"))?;
        event
            .payload
            .as_object_mut()
            .ok_or_else(|| storage("stamped baseline payload is not an object"))?
            .extend(fields.clone());
        baseline
            .validate_fact(event.id, &event.source, event.timestamp, &event.payload)
            .map_err(ContentError::Validation)?;
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(baseline)
    }
}

pub(crate) async fn restore(
    conn: &mut PgConnection,
    baseline: &ManualBaseline,
) -> Result<(), String> {
    // All parents exist before restoring their links. This preserves even
    // a child whose UUID sorts before its parent without relying on row order.
    for raw in &baseline.inventory.sections {
        let mut root = raw.clone();
        root["parent_slug"] = Value::Null;
        sqlx::query("INSERT INTO manual_sections SELECT (jsonb_populate_record(NULL::manual_sections,$1)).*")
            .bind(root).execute(&mut *conn).await.map_err(|e|e.to_string())?;
    }
    for raw in &baseline.inventory.sections {
        sqlx::query("UPDATE manual_sections SET parent_slug=$2 WHERE id=$1")
            .bind(
                raw["id"]
                    .as_str()
                    .ok_or("baseline section has no id")?
                    .parse::<uuid::Uuid>()
                    .map_err(|e| e.to_string())?,
            )
            .bind(raw["parent_slug"].as_str())
            .execute(&mut *conn)
            .await
            .map_err(|e| e.to_string())?;
    }
    for raw in &baseline.inventory.history {
        sqlx::query("INSERT INTO manual_section_history SELECT (jsonb_populate_record(NULL::manual_section_history,$1)).*")
            .bind(raw).execute(&mut *conn).await.map_err(|e|e.to_string())?;
    }
    let current = sequence(conn).await?;
    let target = &baseline.inventory.sequence;
    // Sequence operations are not transactional. Never rewind a consumed
    // gap, including on repeated replay or after a refused transaction.
    if current.last_value < target.last_value
        || (current.last_value == target.last_value && !current.is_called && target.is_called)
    {
        sqlx::query("SELECT setval(pg_get_serial_sequence('manual_section_history','id'),$1,$2)")
            .bind(target.last_value)
            .bind(target.is_called)
            .execute(conn)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
