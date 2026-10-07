//! In-memory adapter for `PeopleRepository`.

use async_trait::async_trait;

use crate::coverage_guard::Change;
use crate::port::{
    PeopleError, PeopleRepository, in_read_order, refuse_another_id, refuse_malformed,
};
use crate::types::Employee;

pub struct InMemoryPeople {
    employees: std::sync::RwLock<Vec<Employee>>,
    recorded: std::sync::Mutex<Vec<boss_core::event::Event>>,
    coverage: Option<std::sync::Arc<dyn crate::coverage_guard::CoverageRead>>,
    /// Explicit key facts for the port double; production reads its
    /// own credential rows inside the guarded transaction.
    keys: Vec<boss_policy_client::coverage::Key>,
}

impl InMemoryPeople {
    pub fn new(employees: Vec<Employee>) -> Self {
        Self {
            employees: std::sync::RwLock::new(employees),
            recorded: std::sync::Mutex::new(Vec::new()),
            coverage: None,
            keys: Vec::new(),
        }
    }

    pub fn with_coverage(
        mut self,
        source: std::sync::Arc<dyn crate::coverage_guard::CoverageRead>,
        keys: Vec<boss_policy_client::coverage::Key>,
    ) -> Self {
        self.coverage = Some(source);
        self.keys = keys;
        self
    }

    /// The coverage basis for one employee write, or `None` when the
    /// guard is not mounted, the write's own existence check will answer,
    /// or the roster and keys show it cannot take a holder away — the
    /// same forecast the Postgres adapter makes before its transaction.
    async fn standing(
        &self,
        id: &str,
        change: Change<'_>,
    ) -> Result<Option<crate::coverage_guard::Standing>, PeopleError> {
        let Some(source) = &self.coverage else {
            return Ok(None);
        };
        let before = crate::coverage_guard::people(
            &self
                .employees
                .read()
                .map_err(|_| PeopleError::Storage("roster lock unavailable".into()))?,
        );
        let Some(after) = crate::coverage_guard::roster_after(&before, id, &change) else {
            return Ok(None);
        };
        crate::coverage_guard::standing_for(
            source.as_ref(),
            &before,
            &self.keys,
            &after,
            &self.keys,
        )
        .await
    }

    /// The judgement under the roster's write lock, on the rows as they
    /// stand there.
    fn judge(
        &self,
        standing: Option<&crate::coverage_guard::Standing>,
        employees: &[Employee],
        id: &str,
        change: Change<'_>,
    ) -> Result<(), PeopleError> {
        if self.coverage.is_none() {
            return Ok(());
        }
        let before = crate::coverage_guard::people(employees);
        let Some(after) = crate::coverage_guard::roster_after(&before, id, &change) else {
            return Ok(());
        };
        crate::coverage_guard::judge_at_commit(standing, &before, &self.keys, &after, &self.keys)
    }

    /// Events the outbox paths recorded — test visibility (the
    /// in-memory analogue of the Pg adapter's in-tx recording).
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded.lock().map(|v| v.clone()).unwrap_or_default()
    }

    fn record(&self, event: boss_core::event::Event) {
        if let Ok(mut v) = self.recorded.lock() {
            v.push(event);
        }
    }
}

/// The roster-wide refusals Postgres states as constraints (the email
/// unique index, the `manager_id` foreign key), stated here as the port
/// words them so the double refuses what production refuses (backlog
/// be459ab9). `emp` is the row about to be written; its own id is not a
/// clash.
fn refuse_unholdable(employees: &[Employee], emp: &Employee) -> Result<(), PeopleError> {
    refuse_malformed(emp)?;
    if let Some(email) = &emp.email {
        let lower = email.to_lowercase();
        if employees.iter().any(|e| {
            e.id != emp.id
                && e.email
                    .as_deref()
                    .is_some_and(|held| held.to_lowercase() == lower)
        }) {
            return Err(PeopleError::Conflict(format!(
                "email `{email}` is held by another employee"
            )));
        }
    }
    if let Some(manager) = &emp.manager_id
        && *manager != emp.id
        && !employees.iter().any(|e| e.id == *manager)
    {
        return Err(PeopleError::Conflict(format!(
            "manager `{manager}` is not an employee"
        )));
    }
    Ok(())
}

#[async_trait]
impl PeopleRepository for InMemoryPeople {
    async fn all_employees(&self) -> Result<Vec<Employee>, PeopleError> {
        let mut all: Vec<Employee> = self
            .employees
            .read()
            .unwrap()
            .iter()
            .cloned()
            .map(in_read_order)
            .collect();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(all)
    }

    async fn employee_by_id(&self, id: &str) -> Result<Option<Employee>, PeopleError> {
        Ok(self
            .employees
            .read()
            .unwrap()
            .iter()
            .find(|e| e.id == id)
            .cloned()
            .map(in_read_order))
    }

    async fn direct_reports(&self, manager_id: &str) -> Result<Vec<Employee>, PeopleError> {
        let mut reports: Vec<Employee> = self
            .employees
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.manager_id.as_deref() == Some(manager_id))
            .cloned()
            .map(in_read_order)
            .collect();
        // Nameless last, as Postgres's ascending NULLS LAST.
        reports.sort_by(|a, b| {
            (a.name.is_none(), &a.name, &a.id).cmp(&(b.name.is_none(), &b.name, &b.id))
        });
        Ok(reports)
    }

    async fn create_employee_at(
        &self,
        emp: &Employee,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, PeopleError> {
        let standing = self.standing(&emp.id, Change::Create(emp)).await?;
        {
            let mut employees = self.employees.write().unwrap();
            if employees.iter().any(|e| e.id == emp.id) {
                return Err(PeopleError::Conflict(format!(
                    "employee {} already exists",
                    emp.id
                )));
            }
            refuse_unholdable(&employees, emp)?;
            self.judge(standing.as_ref(), &employees, &emp.id, Change::Create(emp))?;
            employees.push(emp.clone());
        }
        self.record(stamp.event(
            crate::events::EMPLOYEE_CREATED,
            serde_json::to_value(emp).unwrap_or_default(),
        ));
        Ok(emp.id.clone())
    }

    async fn update_employee_at(
        &self,
        id: &str,
        emp: &Employee,
        _now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), PeopleError> {
        let standing = self.standing(id, Change::Update(emp)).await?;
        {
            let mut employees = self.employees.write().unwrap();
            let pos = employees
                .iter()
                .position(|e| e.id == id)
                .ok_or_else(|| PeopleError::NotFound(id.to_string()))?;
            refuse_another_id(id, emp)?;
            refuse_unholdable(&employees, emp)?;
            self.judge(standing.as_ref(), &employees, id, Change::Update(emp))?;
            employees[pos] = emp.clone();
        }
        self.record(stamp.event(
            crate::events::EMPLOYEE_UPDATED,
            serde_json::to_value(emp).unwrap_or_default(),
        ));
        Ok(())
    }

    async fn delete_employee_at(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), PeopleError> {
        let standing = self.standing(id, Change::Delete).await?;
        {
            let mut employees = self.employees.write().unwrap();
            let pos = employees
                .iter()
                .position(|e| e.id == id)
                .ok_or_else(|| PeopleError::NotFound(id.to_string()))?;
            if let Some(report) = employees
                .iter()
                .filter(|e| e.id != id && e.manager_id.as_deref() == Some(id))
                .map(|e| e.id.as_str())
                .min()
            {
                return Err(PeopleError::Conflict(format!(
                    "employee {id} still manages {report}"
                )));
            }
            self.judge(standing.as_ref(), &employees, id, Change::Delete)?;
            employees.remove(pos);
        }
        self.record(stamp.event(
            crate::events::EMPLOYEE_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    fn test_employee(id: &str, manager: Option<&str>) -> Employee {
        Employee {
            id: id.to_string(),
            name: Some(format!("Test {id}")),
            email: Some(format!("{id}@boss.io")),
            role: Some("service-tech".to_string()),
            department: Some("service".to_string()),
            skill_level: Some(3),
            skills: vec!["network-diagnostics".into()],
            hire_date: Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            location: Some("loc-hq".to_string()),
            manager_id: manager.map(String::from),
            employment_type: Some("full-time".to_string()),
            status: Some("active".to_string()),
            certifications: vec![],
            annual_salary_cents: None,
        }
    }

    fn test_roster() -> InMemoryPeople {
        InMemoryPeople::new(vec![
            test_employee("emp-001", None),
            test_employee("emp-002", Some("emp-001")),
            test_employee("emp-003", Some("emp-001")),
        ])
    }

    #[tokio::test]
    async fn all_employees_returns_all() {
        let repo = test_roster();
        assert_eq!(repo.all_employees().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn find_by_id() {
        let repo = test_roster();
        assert!(repo.employee_by_id("emp-002").await.unwrap().is_some());
        assert!(repo.employee_by_id("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn direct_reports_found() {
        let repo = test_roster();
        let reports = repo.direct_reports("emp-001").await.unwrap();
        assert_eq!(reports.len(), 2);
    }

    #[tokio::test]
    async fn direct_reports_empty() {
        let repo = test_roster();
        assert!(repo.direct_reports("emp-003").await.unwrap().is_empty());
    }
}
