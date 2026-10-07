//! Restore original credential phase receipts without inventing new facts.
use super::{CredentialsError, RotationPhase, receipt::RotationReceipt};
use boss_core::event::Event;
use sqlx::{PgConnection, PgPool};

pub const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("credentials");

async fn restore_in_connection(
    conn: &mut PgConnection,
    event: &Event,
) -> Result<(), CredentialsError> {
    let receipt = RotationReceipt::from_event(event)?;
    let phase = RotationPhase::ALL
        .into_iter()
        .find(|p| p.event_kind() == event.kind)
        .ok_or(CredentialsError::ObservationConflict)?;
    let id = receipt
        .as_ref()
        .map(|r| r.credential_id.as_str())
        .or_else(|| {
            event
                .payload
                .get("credential_id")
                .and_then(serde_json::Value::as_str)
        })
        .ok_or(CredentialsError::ObservationConflict)?;
    let found: Option<String> =
        sqlx::query_scalar("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| CredentialsError::Storage(e.to_string()))?;
    if found.is_none() {
        return Err(CredentialsError::UnknownCredential(id.into()));
    }
    if let Some(receipt) = &receipt {
        let original: Option<String> = sqlx::query_scalar("SELECT receipt_json FROM credential_phase_receipts WHERE credential_id=$1 AND phase=$2 AND observation_id=$3")
            .bind(id).bind(&receipt.phase).bind(&receipt.observation_id).fetch_optional(&mut *conn).await.map_err(|e| CredentialsError::Storage(e.to_string()))?;
        if let Some(original) = original {
            let original: RotationReceipt = serde_json::from_str(&original)
                .map_err(|_| CredentialsError::ObservationConflict)?;
            if original != *receipt {
                return Err(CredentialsError::ObservationConflict);
            }
        } else {
            sqlx::query("INSERT INTO credential_phase_receipts (credential_id,phase,observation_id,receipt_json) VALUES ($1,$2,$3,$4)")
            .bind(id).bind(&receipt.phase).bind(&receipt.observation_id)
            .bind(serde_json::to_string(receipt).map_err(|e| CredentialsError::Storage(e.to_string()))?)
            .execute(&mut *conn).await.map_err(|e| CredentialsError::Storage(e.to_string()))?;
        }
    }
    if phase == RotationPhase::Installed {
        sqlx::query("UPDATE credentials SET rotated_at=$2 WHERE id=$1")
            .bind(id)
            .bind(event.timestamp)
            .execute(&mut *conn)
            .await
            .map_err(|e| CredentialsError::Storage(e.to_string()))?;
    }
    Ok(())
}

/// Preservation replay: registry declarations predate the event-sourced
/// declaration door in some estates. Keep that knowledge and original receipts,
/// restoring missing receipts only from real, complete audit history.
pub async fn rebuild_credentials(pool: &PgPool) -> Result<u64, CredentialsError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| CredentialsError::Storage(e.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REBUILD_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(|e| CredentialsError::Storage(e.to_string()))?;
    boss_events::outbox::lock_and_assert_log_complete(
        &mut tx,
        &["credentials", "credential_phase_receipts"],
    )
    .await
    .map_err(CredentialsError::Storage)?;
    let rows: Vec<(uuid::Uuid, chrono::DateTime<chrono::Utc>, String, String, serde_json::Value)> =
        sqlx::query_as("SELECT event_id,timestamp,source,kind,payload FROM audit_log WHERE kind IN ('credential.declared','credential.minted','credential.installed','credential.verified','credential.revoked') ORDER BY id")
        .fetch_all(&mut *tx).await.map_err(|e| CredentialsError::Storage(e.to_string()))?;
    let count = rows.len() as u64;
    for (id, timestamp, source, kind, payload) in rows {
        if kind == super::types::CREDENTIAL_DECLARED {
            if source != "jobs" {
                return Err(CredentialsError::ObservationConflict);
            }
            let mut declaration = payload;
            let fields = declaration
                .as_object_mut()
                .ok_or(CredentialsError::ObservationConflict)?;
            for key in [
                "declared_by",
                "tenant_id",
                "_actor",
                "_partition",
                "_simulated",
            ] {
                fields.remove(key);
            }
            let row: super::types::CredentialInput = serde_json::from_value(declaration)
                .map_err(|_| CredentialsError::ObservationConflict)?;
            super::types::validate_credential(&row)
                .map_err(CredentialsError::InvalidObservation)?;
            sqlx::query("INSERT INTO credentials(id,kind,issuer,principal,scopes,storage_location,consumers,rotation_policy,notes) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(id) DO NOTHING")
                .bind(&row.id).bind(&row.kind).bind(&row.issuer).bind(&row.principal)
                .bind(serde_json::to_value(&row.scopes).map_err(|e| CredentialsError::Storage(e.to_string()))?)
                .bind(&row.storage_location).bind(serde_json::to_value(&row.consumers).map_err(|e| CredentialsError::Storage(e.to_string()))?)
                .bind(&row.rotation_policy).bind(&row.notes).execute(&mut *tx).await.map_err(|e| CredentialsError::Storage(e.to_string()))?;
            continue;
        }
        restore_in_connection(
            &mut tx,
            &Event {
                id,
                timestamp,
                source,
                kind,
                payload,
            },
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|e| CredentialsError::Storage(e.to_string()))?;
    Ok(count)
}
