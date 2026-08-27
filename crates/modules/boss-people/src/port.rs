//! Hexagonal port: `PeopleRepository` defines what the domain needs from
//! persistence.

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

use crate::types::Employee;

#[derive(Debug, thiserror::Error)]
pub enum PeopleError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// Persistence port for the employee roster.
///
/// Mutation methods come in two flavors: a convenience overload
/// that stamps `Utc::now()` server-side, and an `_at` variant.
/// Handlers that emit a domain event for the same mutation use
/// `_at` so the projection write and the event share one
/// timestamp — required for the audit_log → projection rebuild
/// path to reproduce timestamps. See
/// `docs/design/projection-rebuilders.md`.
#[async_trait]
pub trait PeopleRepository: Send + Sync {
    /// Return every employee.
    async fn all_employees(&self) -> Result<Vec<Employee>, PeopleError>;

    /// Return a single employee by ID, or `None` if not found.
    async fn employee_by_id(&self, id: &str) -> Result<Option<Employee>, PeopleError>;

    /// Return direct reports for a manager.
    async fn direct_reports(&self, manager_id: &str) -> Result<Vec<Employee>, PeopleError>;

    /// Create a new employee. Returns the ID. Errors if ID already exists.
    /// OUTBOX (phase 2): records `people.employee.created` (full row
    /// state) in the same transaction as the row.
    async fn create_employee(&self, emp: &Employee) -> Result<String, PeopleError> {
        let stamp = EventStamp::new("people", ActorId::Automation("platform".into()));
        self.create_employee_at(emp, stamp.timestamp, &stamp).await
    }
    async fn create_employee_at(
        &self,
        emp: &Employee,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<String, PeopleError>;

    /// Replace an employee by ID. Errors if ID doesn't exist.
    /// Records `people.employee.updated` (full row state) in-tx.
    async fn update_employee(&self, id: &str, emp: &Employee) -> Result<(), PeopleError> {
        let stamp = EventStamp::new("people", ActorId::Automation("platform".into()));
        self.update_employee_at(id, emp, stamp.timestamp, &stamp)
            .await
    }
    async fn update_employee_at(
        &self,
        id: &str,
        emp: &Employee,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), PeopleError>;

    /// Delete an employee and satellite data. Errors if ID doesn't exist.
    /// Records `people.employee.deleted` (`{id, deleted_at}`) in-tx.
    async fn delete_employee(&self, id: &str) -> Result<(), PeopleError> {
        let stamp = EventStamp::new("people", ActorId::Automation("platform".into()));
        self.delete_employee_at(id, stamp.timestamp, &stamp).await
    }
    async fn delete_employee_at(
        &self,
        id: &str,
        now: DateTime<Utc>,
        stamp: &EventStamp,
    ) -> Result<(), PeopleError>;
}
