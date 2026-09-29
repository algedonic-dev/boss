//! Where an employee's `department` is checked: the DEPARTMENTS
//! REGISTRY, the one list of what a department IS (backlog c87e3d6d,
//! decided 2026-09-27 on page audit 9f7ba57d).
//!
//! THE FACT THAT LIVED TWICE (CLAUDE.md §9a). Until this module an
//! employee's `department` was validated against the `employee` Class
//! drawer — `(employee, code)` rows whose `member_attribute` is
//! `department` — while every protocol reader (readiness, the weekly
//! retro, routing, the chrome bar) read the `departments` table behind
//! `GET /api/departments`. Measured live on 2026-09-27: 9 department
//! Classes against 10 registry departments, 7 shared, and the
//! founder's row named `operations`, a department only the Classes
//! held. Two lists, drifting, and the roster answered "which
//! department" from the one nothing else read. So the column now
//! validates against the registry, and the department Classes retire.
//!
//! WHY A SQL READ AND NOT AN HTTP CLIENT. The `departments` table sits
//! in the database this service already holds a pool on, the question
//! is one row's existence, and boss-ledger's `revenue_accounts` reads
//! the `classes` table the same way. An HTTP hop would add a signed
//! caller, a URL and a failure mode to answer the same `EXISTS`.

use async_trait::async_trait;

use crate::port::PeopleError;

/// The departments an employee may sit in: the registry's un-retired
/// rows, by code.
#[async_trait]
pub trait DepartmentRoster: Send + Sync {
    /// Whether `code` is an active department. `Err` is the registry
    /// not answering — a different fact from "not held".
    async fn holds(&self, code: &str) -> Result<bool, String>;
}

/// Reject a write whose `department` is not an active department in
/// the registry, naming the code and where it looked.
pub async fn validate_department(
    roster: &dyn DepartmentRoster,
    code: &str,
) -> Result<(), PeopleError> {
    let held = roster
        .holds(code)
        .await
        .map_err(|e| PeopleError::Storage(format!("departments registry: {e}")))?;
    if !held {
        return Err(PeopleError::Conflict(format!(
            "department `{code}` is not an active department in the departments registry \
             (GET /api/departments) — declare it in the tenant's seeds/departments.toml first"
        )));
    }
    Ok(())
}

/// The `departments` table, read on the service's own pool.
#[cfg(feature = "postgres")]
pub struct PgDepartmentRoster {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PgDepartmentRoster {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[cfg(feature = "postgres")]
#[async_trait]
impl DepartmentRoster for PgDepartmentRoster {
    async fn holds(&self, code: &str) -> Result<bool, String> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM departments WHERE id = $1 AND retired_at IS NULL)",
        )
        .bind(code)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| e.to_string())
    }
}

/// A fixed roster — the in-memory double for tests.
pub struct FixedDepartments(pub Vec<String>);

#[async_trait]
impl DepartmentRoster for FixedDepartments {
    async fn holds(&self, code: &str) -> Result<bool, String> {
        Ok(self.0.iter().any(|c| c == code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> FixedDepartments {
        FixedDepartments(vec!["it".into(), "executive".into()])
    }

    #[tokio::test]
    async fn a_registry_department_is_accepted() {
        assert!(validate_department(&roster(), "executive").await.is_ok());
    }

    /// The founder's row on 2026-09-27: `operations` was an employee
    /// department Class and never a department.
    #[tokio::test]
    async fn a_code_the_registry_does_not_hold_is_refused_naming_the_registry() {
        match validate_department(&roster(), "operations").await {
            Err(PeopleError::Conflict(msg)) => assert!(
                msg.contains("`operations`") && msg.contains("departments registry"),
                "the refusal names the code and the registry: {msg}"
            ),
            other => panic!("`operations` must be refused, got {other:?}"),
        }
    }

    struct Dark;
    #[async_trait]
    impl DepartmentRoster for Dark {
        async fn holds(&self, _: &str) -> Result<bool, String> {
            Err("connection refused".into())
        }
    }

    #[tokio::test]
    async fn a_registry_that_cannot_answer_is_not_a_refusal() {
        assert!(matches!(
            validate_department(&Dark, "it").await,
            Err(PeopleError::Storage(m)) if m.contains("departments registry")
        ));
    }
}
