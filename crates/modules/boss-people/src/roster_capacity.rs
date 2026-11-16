//! The people-owned snapshot projects only Class attributes, never profiles.
use boss_core::roster_capacity::{
    ClassRequirement, RosterSnapshot, RosterSnapshotError, validate_eligibility,
};
use std::collections::{BTreeMap, BTreeSet};

fn unavailable(query: &str) -> RosterSnapshotError {
    RosterSnapshotError::Unavailable(format!("people {query} unreadable"))
}

pub(crate) async fn snapshot(
    pool: &sqlx::PgPool,
    eligibility: &[ClassRequirement],
) -> Result<RosterSnapshot, RosterSnapshotError> {
    validate_eligibility(eligibility).map_err(RosterSnapshotError::Invalid)?;
    if eligibility
        .iter()
        .any(|r| !matches!(r.attribute.as_str(), "role" | "status" | "employment_type"))
    {
        return Err(RosterSnapshotError::Invalid(
            "unsupported employee Class attribute".into(),
        ));
    }
    let mut tx = pool.begin().await.map_err(|_| unavailable("snapshot"))?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(|_| unavailable("snapshot isolation"))?;
    let classes: Vec<(String, String)> = sqlx::query_as("SELECT member_attribute, code FROM classes WHERE subject_kind='employee' AND retired_at IS NULL AND member_attribute IN ('role','status','employment_type')")
        .fetch_all(&mut *tx).await.map_err(|_| unavailable("Classes"))?;
    let mut valid: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (axis, code) in classes {
        valid.entry(axis).or_default().insert(code);
    }
    if eligibility.iter().any(|r| {
        r.codes.iter().any(|code| {
            !valid
                .get(&r.attribute)
                .is_some_and(|codes| codes.contains(code))
        })
    }) {
        return Err(RosterSnapshotError::Invalid(
            "eligibility names an unavailable employee Class".into(),
        ));
    }
    // Narrow DTO: no person identity, name, salary, skills, passkeys or agents.
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT role,status,employment_type FROM employees")
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| unavailable("Class rows"))?;
    let revision: Option<i64> =
        sqlx::query_scalar("SELECT MAX(id) FROM audit_log WHERE source='people'")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| unavailable("audit basis"))?;
    let pending = boss_events::outbox::source_undrained_count(&mut *tx, "people")
        .await
        .map_err(|_| unavailable("outbox"))?;
    if pending != 0 {
        return Err(RosterSnapshotError::Unavailable(
            "people outbox undrained".into(),
        ));
    }
    let mut count = 0u64;
    for (role, status, employment_type) in rows {
        let mut excluded = false;
        let mut unknown = false;
        for requirement in eligibility {
            let value = match requirement.attribute.as_str() {
                "role" => role.as_ref(),
                "status" => status.as_ref(),
                "employment_type" => employment_type.as_ref(),
                _ => None,
            };
            match value {
                Some(code)
                    if valid
                        .get(&requirement.attribute)
                        .is_some_and(|codes| codes.contains(code)) =>
                {
                    excluded |= !requirement.codes.contains(code);
                }
                _ => unknown = true,
            }
        }
        if !excluded {
            if unknown {
                return Err(RosterSnapshotError::Unavailable(
                    "employee eligibility unavailable".into(),
                ));
            }
            count = count.checked_add(1).ok_or_else(|| unavailable("count"))?;
        }
    }
    let observation = RosterSnapshot {
        eligibility: eligibility.to_vec(),
        revision,
        count,
    };
    observation
        .validate()
        .map_err(|_| unavailable("audit basis"))?;
    tx.commit()
        .await
        .map_err(|_| unavailable("snapshot completion"))?;
    Ok(observation)
}
