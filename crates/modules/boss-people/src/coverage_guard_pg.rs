//! The one local serialization boundary used by roster and key guards.

use boss_policy_client::AccessTier;
use boss_policy_client::coverage::{Key, Person};
use sqlx::{Postgres, Transaction};

use crate::port::PeopleError;

pub struct Local {
    pub roster: Vec<Person>,
    pub keys: Vec<Key>,
}

/// Lock before reading either table. Every guarded local mutation
/// takes this same lock; a key removal and a role retirement cannot
/// each judge the other person's pre-write hold and both commit.
pub async fn read_locked(tx: &mut Transaction<'_, Postgres>) -> Result<Local, PeopleError> {
    lock(tx).await?;
    read(tx).await
}

/// The same two reads with no lock, on a pooled connection: what a
/// write consults BEFORE its transaction to decide whether the coverage
/// basis must be read at all (`coverage_guard::can_orphan`). It is a
/// forecast only — the decision is made again under the lock, on
/// [`read_locked`]'s rows, and a forecast the locked rows contradict
/// refuses the write (review c3b96c09 F1, 2026-10-06).
pub async fn read_unlocked(pool: &sqlx::PgPool) -> Result<Local, PeopleError> {
    let mut connection = pool
        .acquire()
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;
    read(&mut connection).await
}

pub async fn lock(tx: &mut Transaction<'_, Postgres>) -> Result<(), PeopleError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('boss.people.coverage-guard', 0))")
        .execute(&mut **tx)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;
    Ok(())
}

async fn read(tx: &mut sqlx::PgConnection) -> Result<Local, PeopleError> {
    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
        Option<chrono::NaiveDate>,
    )> = sqlx::query_as(
        "SELECT id, role, status, hire_date FROM employees ORDER BY id COLLATE \"C\"",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| PeopleError::Storage(e.to_string()))?;
    let roster = rows
        .into_iter()
        .map(|(id, role, status, hire_date)| Person {
            id,
            role,
            active: status.as_deref() == Some("active"),
            hire_date,
        })
        .collect();
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT employee_id, access_tier FROM webauthn_credentials ORDER BY employee_id COLLATE \"C\", credential_id")
        .fetch_all(&mut *tx).await.map_err(|e| PeopleError::Storage(e.to_string()))?;
    let keys = rows
        .into_iter()
        .map(|(employee_id, tier)| {
            let access_tier: AccessTier = serde_json::from_value(serde_json::Value::String(tier))
                .map_err(|e| {
                PeopleError::Unavailable(format!("stored passkey tier cannot be judged: {e}"))
            })?;
            Ok(Key {
                employee_id,
                access_tier,
            })
        })
        .collect::<Result<_, PeopleError>>()?;
    Ok(Local { roster, keys })
}
