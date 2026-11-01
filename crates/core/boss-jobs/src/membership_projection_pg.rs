//! PostgreSQL generation adapter: CAS, changed rows and watermark share
//! one transaction. Current state and bounded original pruned-census/registry
//! evidence are retained, rather than packet event history.
use crate::membership_projection::publication_check;
use crate::{
    membership_projection::{
        CanonicalWindows, CheckpointRows, MembershipProjectionRepository, ProjectionIdentity,
        ProjectionWindows, StoredProjection, storage,
    },
    membership_replay::projection::MembershipProjection,
    port::JobsError,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeMap;

/// Bound each derived-row SQL statement independently of the checkpoint size.
pub const PUBLICATION_BATCH_ROWS: usize = 512;
/// Preferred encoded array size; a domain-valid larger row remains a singleton.
pub const PUBLICATION_BATCH_BYTES: usize = 1024 * 1024;
// Generated bucket/UUID-or-ordinal key wrapper; payload keeps its existing domain cap.
const PUBLICATION_ROW_ENVELOPE_BYTES: usize = 256;

pub struct PgMembershipProjection {
    pool: PgPool,
}

impl PgMembershipProjection {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl MembershipProjectionRepository for PgMembershipProjection {
    async fn read_windows(
        &self,
        identity: &ProjectionIdentity,
        hours: i64,
        _observed_at: chrono::DateTime<chrono::Utc>,
        binding: &crate::membership_replay::projection::ProjectionBinding,
    ) -> Result<Option<ProjectionWindows>, JobsError> {
        if !matches!(hours, 24 | 48) {
            return Err(storage(
                "membership projection window must be 24 or 48 hours",
            ));
        }
        let key = identity.key()?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("SET LOCAL statement_timeout = '2s'")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let row: Option<(i64, Value, Value)> = sqlx::query_as("SELECT revision,header,canonical_windows FROM membership_projection_generations WHERE identity=$1")
            .bind(&key).fetch_optional(&mut *tx).await.map_err(storage)?;
        let Some((revision, header, windows)) = row else {
            return Ok(None);
        };
        let stored_binding: crate::membership_replay::projection::ProjectionBinding =
            serde_json::from_value(
                header
                    .get("binding")
                    .cloned()
                    .ok_or_else(|| storage("checkpoint binding unavailable"))?,
            )
            .map_err(storage)?;
        if stored_binding != *binding
            || stored_binding.actor != identity.actor
            || stored_binding.scope != identity.scope
        {
            return Err(storage(
                "caller, scope or current authorized census changed",
            ));
        }
        let until = serde_json::from_value(
            header
                .get("until")
                .cloned()
                .ok_or_else(|| storage("native wall cutoff unavailable"))?,
        )
        .map_err(storage)?;
        let cutoff = header
            .get("audit_cutoff")
            .and_then(Value::as_i64)
            .ok_or_else(|| storage("native audit cutoff unavailable"))?;
        let canonical: CanonicalWindows = serde_json::from_value(windows).map_err(storage)?;
        let reading = canonical.windows(revision, hours, until, cutoff)?;
        let basis = CheckpointRows {
            header,
            rows: BTreeMap::new(),
        }
        .join()?;
        basis
            .validate_window(reading.this.since, binding)
            .map_err(storage)?;
        if let Ok(wide) = &reading.both {
            basis
                .validate_window(wide.since, binding)
                .map_err(storage)?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(Some(reading))
    }
    async fn read(
        &self,
        identity: &ProjectionIdentity,
        since: chrono::DateTime<chrono::Utc>,
        binding: &crate::membership_replay::projection::ProjectionBinding,
    ) -> Result<Option<crate::membership_projection::ProjectionReading>, JobsError> {
        let key = identity.key()?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("SET LOCAL statement_timeout = '2s'")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let header: Option<(i64, Value)> = sqlx::query_as(
            "SELECT revision,header FROM membership_projection_generations WHERE identity=$1",
        )
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let Some((revision, header)) = header else {
            return Ok(None);
        };
        let rows: Vec<(String, String, Value)> = sqlx::query_as("SELECT bucket,row_key,payload FROM membership_projection_rows WHERE identity=$1 AND ((bucket='fact' AND happened_at >= $2) OR (bucket='span' AND happened_at < $2 AND ended_at >= $2)) ORDER BY bucket,row_key LIMIT 250001")
            .bind(&key).bind(since).fetch_all(&mut *tx).await.map_err(storage)?;
        if rows.len() > 250_000 {
            return Err(storage("compact crossing read exceeds bound"));
        }
        let rows = rows.into_iter().map(|(b, k, v)| ((b, k), v)).collect();
        let checkpoint = CheckpointRows { header, rows };
        checkpoint.require_identity(identity)?;
        let reading = checkpoint.join()?.read(since, binding).map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(Some(crate::membership_projection::ProjectionReading {
            revision,
            reading,
        }))
    }
    async fn load(
        &self,
        identity: &ProjectionIdentity,
    ) -> Result<Option<StoredProjection>, JobsError> {
        let key = identity.key()?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let header: Option<(i64, Value)> = sqlx::query_as(
            "SELECT revision,header FROM membership_projection_generations WHERE identity=$1",
        )
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let Some((revision, header)) = header else {
            return Ok(None);
        };
        let rows: Vec<(String, String, Value)> = sqlx::query_as("SELECT bucket,row_key,payload FROM membership_projection_rows WHERE identity=$1 ORDER BY bucket,row_key")
            .bind(&key).fetch_all(&mut *tx).await.map_err(storage)?;
        let rows = rows.into_iter().map(|(b, k, v)| ((b, k), v)).collect();
        let checkpoint = CheckpointRows { header, rows };
        checkpoint.require_identity(identity)?;
        let checkpoint = checkpoint.join()?;
        tx.commit().await.map_err(storage)?;
        Ok(Some(StoredProjection {
            revision,
            checkpoint,
        }))
    }

    async fn commit_with_budget(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
        budget: &crate::membership_replay::ReplayBudget,
    ) -> Result<Option<i64>, JobsError> {
        self.publish(identity, previous, next, Some(budget)).await
    }
    async fn commit(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
    ) -> Result<Option<i64>, JobsError> {
        self.publish(identity, previous, next, None).await
    }
}

impl PgMembershipProjection {
    async fn publish(
        &self,
        identity: &ProjectionIdentity,
        previous: Option<&StoredProjection>,
        next: &MembershipProjection,
        budget: Option<&crate::membership_replay::ReplayBudget>,
    ) -> Result<Option<i64>, JobsError> {
        publication_check(budget)?;
        let key = identity.key()?;
        let canonical_windows =
            serde_json::to_value(CanonicalWindows::from_checkpoint(next)).map_err(storage)?;
        let next = CheckpointRows::split(next)?;
        next.require_identity(identity)?;
        let old = previous
            .map(|p| CheckpointRows::split(&p.checkpoint))
            .transpose()?;
        let next_digest = next.basis_digest()?;
        let old_digest = old.as_ref().map(CheckpointRows::basis_digest).transpose()?;
        if let Some(old) = &old {
            old.require_identity(identity)?;
            let cutoff = |value: &Value| {
                value
                    .get("audit_cutoff")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| storage("native audit cutoff unavailable"))
            };
            let until = |value: &Value| {
                serde_json::from_value::<chrono::DateTime<chrono::Utc>>(
                    value
                        .get("until")
                        .cloned()
                        .ok_or_else(|| storage("native wall cutoff unavailable"))?,
                )
                .map_err(storage)
            };
            if cutoff(&next.header)? < cutoff(&old.header)?
                || until(&next.header)? < until(&old.header)?
            {
                return Err(storage("native generation watermark cannot move backwards"));
            }
        }
        let unchanged = old
            .as_ref()
            .is_some_and(|old| old.header == next.header && old.rows == next.rows);
        let empty = BTreeMap::new();
        let old_rows = old.as_ref().map_or(&empty, |p| &p.rows);
        let removed = encoded_publication_batches(
            old_rows
                .keys()
                .filter(|key| !next.rows.contains_key(*key))
                .map(|(bucket, key)| json!({"bucket":bucket,"row_key":key})),
            PUBLICATION_BATCH_ROWS,
            PUBLICATION_BATCH_BYTES,
            crate::membership_replay::projection::MAX_CHECKPOINT_BYTES
                + PUBLICATION_ROW_ENVELOPE_BYTES,
            budget,
        )?;
        // Consume changed payloads; transport preparation never clones an entire row ledger.
        let changed = encoded_publication_batches(
            next.rows
                .into_iter()
                .filter(|(key, value)| old_rows.get(key) != Some(value))
                .map(|((bucket, key), payload)| {
                    Value::Object(serde_json::Map::from_iter([
                        ("bucket".into(), Value::String(bucket)),
                        ("row_key".into(), Value::String(key)),
                        ("payload".into(), payload),
                    ]))
                }),
            PUBLICATION_BATCH_ROWS,
            PUBLICATION_BATCH_BYTES,
            crate::membership_replay::projection::MAX_CHECKPOINT_BYTES
                + PUBLICATION_ROW_ENVELOPE_BYTES,
            budget,
        )?;
        publication_check(budget)?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let outcome: Result<Option<i64>, JobsError> = async {
            publication_timeout(&mut tx, budget).await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext('membership_projection_generations'))")
                .execute(&mut *tx).await.map_err(storage)?;
            publication_timeout(&mut tx, budget).await?;
            sqlx::query("INSERT INTO membership_projection_generations(identity,revision,header,canonical_windows) VALUES($1,0,'{}','{}') ON CONFLICT DO NOTHING")
                .bind(&key).execute(&mut *tx).await.map_err(storage)?;
            publication_timeout(&mut tx, budget).await?;
            let (revision, header, basis_digest): (i64, Value, Option<String>) = sqlx::query_as("SELECT revision,header,basis_digest FROM membership_projection_generations WHERE identity=$1 FOR UPDATE")
                .bind(&key).fetch_one(&mut *tx).await.map_err(storage)?;
            if revision != previous.map_or(0, |value| value.revision) { return Ok(None); }
            let legacy_basis = basis_digest.is_none();
            if let Some(old) = &old {
                // Reused revision numbers do not prove that the persisted row
                // ledger is the one against which this delta was prepared.
                if let Some(digest) = basis_digest {
                    if Some(&digest) != old_digest.as_ref() { return Ok(None); }
                } else {
                    // Legacy generations have no digest. Compare their complete
                    // actual basis under the same lock before recording a witness.
                    publication_timeout(&mut tx, budget).await?;
                    let rows: Vec<(String,String,Value)> = sqlx::query_as("SELECT bucket,row_key,payload FROM membership_projection_rows WHERE identity=$1 ORDER BY bucket,row_key")
                        .bind(&key).fetch_all(&mut *tx).await.map_err(storage)?;
                    let rows = rows.into_iter().map(|(bucket,key,value)|((bucket,key),value)).collect::<BTreeMap<_,_>>();
                    if header != old.header || rows != old.rows { return Ok(None); }
                }
            }
            if unchanged {
                if legacy_basis {
                    publication_timeout(&mut tx, budget).await?;
                    sqlx::query("UPDATE membership_projection_generations SET basis_digest=$2 WHERE identity=$1")
                        .bind(&key).bind(&next_digest).execute(&mut *tx).await.map_err(storage)?;
                }
                return Ok(Some(revision));
            }
            let revision = revision.checked_add(1).ok_or_else(|| storage("generation revision exhausted"))?;
            for batch in removed {
                publication_timeout(&mut tx, budget).await?;
                sqlx::query("DELETE FROM membership_projection_rows r USING jsonb_to_recordset($2::text::jsonb) AS d(bucket text,row_key text) WHERE r.identity=$1 AND r.bucket=d.bucket AND r.row_key=d.row_key")
                    .bind(&key).bind(batch).execute(&mut *tx).await.map_err(storage)?;
                publication_check(budget)?;
            }
            for batch in changed {
                publication_timeout(&mut tx, budget).await?;
                sqlx::query("INSERT INTO membership_projection_rows(identity,bucket,row_key,payload,happened_at,ended_at) SELECT $1,d.bucket,d.row_key,d.payload,CASE WHEN d.bucket='fact' THEN (d.payload->>'at')::timestamptz WHEN d.bucket='span' THEN (d.payload->>0)::timestamptz END,CASE WHEN d.bucket='span' THEN (d.payload->>1)::timestamptz END FROM jsonb_to_recordset($2::text::jsonb) AS d(bucket text,row_key text,payload jsonb) ON CONFLICT(identity,bucket,row_key) DO UPDATE SET payload=EXCLUDED.payload,happened_at=EXCLUDED.happened_at,ended_at=EXCLUDED.ended_at WHERE membership_projection_rows.payload IS DISTINCT FROM EXCLUDED.payload")
                    .bind(&key).bind(batch).execute(&mut *tx).await.map_err(storage)?;
                publication_check(budget)?;
            }
            publication_timeout(&mut tx, budget).await?;
            sqlx::query("UPDATE membership_projection_generations SET revision=$2,header=$3,canonical_windows=$4,basis_digest=$5,committed_at=clock_timestamp() WHERE identity=$1")
                .bind(&key).bind(revision).bind(next.header).bind(canonical_windows).bind(next_digest).execute(&mut *tx).await.map_err(storage)?;
            publication_timeout(&mut tx, budget).await?;
            let evicted: Vec<String> = sqlx::query_scalar("SELECT identity FROM membership_projection_generations WHERE identity<>$1 ORDER BY committed_at DESC,identity DESC OFFSET $2")
                .bind(&key).bind((crate::membership_projection::MAX_GENERATIONS - 1) as i64).fetch_all(&mut *tx).await.map_err(storage)?;
            for identity in evicted {
                loop {
                    publication_timeout(&mut tx, budget).await?;
                    let keys: Vec<(String,String)> = sqlx::query_as("SELECT bucket,row_key FROM membership_projection_rows WHERE identity=$1 ORDER BY bucket,row_key LIMIT $2")
                        .bind(&identity).bind(PUBLICATION_BATCH_ROWS as i64).fetch_all(&mut *tx).await.map_err(storage)?;
                    if keys.is_empty() { break; }
                    let batches = encoded_publication_batches(keys.into_iter().map(|(bucket,key)|json!({"bucket":bucket,"row_key":key})),PUBLICATION_BATCH_ROWS,PUBLICATION_BATCH_BYTES,
                        crate::membership_replay::projection::MAX_CHECKPOINT_BYTES + PUBLICATION_ROW_ENVELOPE_BYTES,budget)?;
                    for batch in batches {
                        publication_timeout(&mut tx, budget).await?;
                        sqlx::query("DELETE FROM membership_projection_rows r USING jsonb_to_recordset($2::text::jsonb) AS d(bucket text,row_key text) WHERE r.identity=$1 AND r.bucket=d.bucket AND r.row_key=d.row_key")
                            .bind(&identity).bind(batch).execute(&mut *tx).await.map_err(storage)?;
                        publication_check(budget)?;
                    }
                }
                // Header last: its cascade has no remaining row ledger to delete.
                publication_timeout(&mut tx, budget).await?;
                sqlx::query("DELETE FROM membership_projection_generations WHERE identity=$1")
                    .bind(&identity).execute(&mut *tx).await.map_err(storage)?;
            }
            publication_check(budget)?;
            Ok(Some(revision))
        }.await;
        match outcome {
            Ok(Some(revision)) => {
                if let Err(error) = publication_timeout(&mut tx, budget).await {
                    tx.rollback().await.map_err(|rollback| {
                        storage(format!(
                            "{error}; rollback acknowledgement failed: {rollback}"
                        ))
                    })?;
                    return Err(error);
                }
                // Once COMMIT is sent, an absent acknowledgement is uncertain,
                // never evidence that the old generation was restored.
                tx.commit().await.map_err(|error| {
                    storage(format!(
                        "projection COMMIT acknowledgement failed; outcome uncertain: {error}"
                    ))
                })?;
                Ok(Some(revision))
            }
            Ok(None) => {
                tx.rollback().await.map_err(storage)?;
                Ok(None)
            }
            Err(error) => {
                tx.rollback().await.map_err(|rollback| {
                    storage(format!(
                        "{error}; rollback acknowledgement failed: {rollback}"
                    ))
                })?;
                Err(error)
            }
        }
    }
}

async fn publication_timeout(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    budget: Option<&crate::membership_replay::ReplayBudget>,
) -> Result<(), JobsError> {
    let statement_limit = std::time::Duration::from_secs(5);
    let remaining = match budget {
        Some(budget) => budget.remaining().map_err(storage)?.min(statement_limit),
        None => statement_limit,
    };
    publication_check(budget)?;
    let milliseconds = remaining.as_millis().max(1);
    sqlx::query("SELECT set_config('statement_timeout',$1,true)")
        .bind(format!("{milliseconds}ms"))
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    publication_check(budget)
}

fn encoded_publication_batches(
    rows: impl IntoIterator<Item = Value>,
    max_rows: usize,
    preferred_bytes: usize,
    hard_row_bytes: usize,
    budget: Option<&crate::membership_replay::ReplayBudget>,
) -> Result<Vec<String>, JobsError> {
    if max_rows == 0 || preferred_bytes < 2 {
        return Err(storage("invalid bounded publication batch limits"));
    }
    let mut batches = Vec::new();
    let mut current = vec![b'['];
    let mut count = 0;
    for row in rows {
        publication_check(budget)?;
        let encoded = serde_json::to_vec(&row).map_err(storage)?;
        let singleton = encoded
            .len()
            .checked_add(2)
            .ok_or_else(|| storage("publication row encoded length overflow"))?;
        if singleton > hard_row_bytes {
            return Err(storage(
                "publication row exceeds the domain checkpoint bound plus its bounded row envelope",
            ));
        }
        let projected = current
            .len()
            .checked_add(encoded.len())
            .and_then(|size| size.checked_add(usize::from(count != 0) + 1))
            .ok_or_else(|| storage("publication batch encoded length overflow"))?;
        if count != 0 && (count == max_rows || projected > preferred_bytes) {
            current.push(b']');
            batches.push(String::from_utf8(current).map_err(storage)?);
            current = vec![b'['];
            count = 0;
        }
        if count != 0 {
            current.push(b',');
        }
        current.extend(encoded);
        count += 1;
    }
    if count != 0 {
        current.push(b']');
        batches.push(String::from_utf8(current).map_err(storage)?);
    }
    publication_check(budget)?;
    Ok(batches)
}

#[cfg(test)]
mod publication_batch_tests {
    use super::*;

    #[test]
    fn encoded_chunks_keep_every_original_row_and_exact_json_bytes() {
        let rows = vec![
            json!({"a":"λ\"\\\n"}),
            json!({"b":[1,2,3]}),
            json!({"c":true}),
            json!({"d":null}),
            json!({"e":{}}),
        ];
        let batches = encoded_publication_batches(rows.clone(), 2, 80, 256, None).unwrap();
        let mut observed = Vec::new();
        for batch in batches {
            let values: Vec<Value> = serde_json::from_str(&batch).unwrap();
            assert!(values.len() <= 2);
            assert_eq!(batch.as_bytes(), serde_json::to_vec(&values).unwrap());
            assert!(batch.len() <= 80 || values.len() == 1);
            observed.extend(values);
        }
        assert_eq!(observed, rows);
    }

    #[test]
    fn a_domain_valid_oversized_row_is_a_singleton_not_a_new_refusal() {
        let rows = vec![
            json!({"payload":"x".repeat(96)}),
            json!({"payload":"small"}),
        ];
        let batches = encoded_publication_batches(rows.clone(), 512, 32, 256, None).unwrap();
        assert_eq!(batches.len(), 2);
        assert!(batches[0].len() > 32);
        let observed: Vec<Value> = batches
            .iter()
            .flat_map(|batch| serde_json::from_str::<Vec<Value>>(batch).unwrap())
            .collect();
        assert_eq!(observed, rows);
        assert!(
            encoded_publication_batches(vec![json!("x".repeat(300))], 512, 32, 256, None)
                .unwrap_err()
                .to_string()
                .contains("domain checkpoint bound")
        );
    }

    #[test]
    fn array_framing_and_exact_byte_boundary_are_counted() {
        let rows = vec![json!("a"), json!("b")];
        let exact = serde_json::to_vec(&rows).unwrap().len();
        assert_eq!(
            encoded_publication_batches(rows.clone(), 512, exact, 256, None)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            encoded_publication_batches(rows, 512, exact - 1, 256, None)
                .unwrap()
                .len(),
            2
        );
        assert!(
            encoded_publication_batches(Vec::<Value>::new(), 512, 32, 256, None)
                .unwrap()
                .is_empty()
        );
    }
}
