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
    #[error("coverage unavailable: {0}")]
    Unavailable(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

/// An employee as every read answers it: skills in byte order,
/// certifications by `issued_on` then `name` in byte order (ties in the
/// order written). Both adapters answer through this one statement —
/// until the adapters-agree suite (backlog be459ab9) Postgres sorted by
/// the database's locale and the double answered whatever order it was
/// handed, so the same employee read back two ways.
pub fn in_read_order(mut emp: Employee) -> Employee {
    emp.skills.sort();
    emp.certifications
        .sort_by(|a, b| (a.issued_on, &a.name).cmp(&(b.issued_on, &b.name)));
    emp
}

/// What no roster can hold in one row, refused as a `Conflict` naming
/// the value before either adapter writes: a skill level outside the
/// schema's `1..=5` CHECK, or a skill listed twice (the skills table's
/// primary key). Postgres refused both as a `Storage` error — a 500 —
/// and the double accepted both (backlog be459ab9).
pub fn refuse_malformed(emp: &Employee) -> Result<(), PeopleError> {
    if let Some(level) = emp.skill_level.filter(|l| !(1..=5).contains(l)) {
        return Err(PeopleError::Conflict(format!(
            "skill_level {level} is outside 1..=5"
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    if let Some(twice) = emp.skills.iter().find(|s| !seen.insert(s.as_str())) {
        return Err(PeopleError::Conflict(format!(
            "skill `{twice}` is listed twice"
        )));
    }
    Ok(())
}

/// An update names its row twice — the id it is called with and the
/// body's own `id` — and the two must agree. Until backlog be459ab9 the
/// double replaced the named row with the body (renaming its identity)
/// while Postgres upserted the BODY's id, so one call changed two
/// different employees depending on the adapter.
pub fn refuse_another_id(id: &str, emp: &Employee) -> Result<(), PeopleError> {
    if emp.id != id {
        return Err(PeopleError::Conflict(format!(
            "the body names employee {} but the update names {id}",
            emp.id
        )));
    }
    Ok(())
}

/// Persistence port for the employee roster.
///
/// Every read answers employees through [`in_read_order`]; the roster
/// is ordered by `id` and direct reports by `name` (nameless last, then
/// `id`), all in BYTE order. A write is refused as a `Conflict` naming
/// the value when the roster cannot hold it: [`refuse_malformed`], an
/// email another employee holds (case-insensitively — credentials key
/// on it), a `manager_id` naming no employee (an employee may manage
/// itself), an update whose body names another id, and a delete of an
/// employee who still manages someone. Each was a `Storage` error in
/// Postgres and accepted by the double until backlog be459ab9.
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
