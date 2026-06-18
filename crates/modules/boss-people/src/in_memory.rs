//! In-memory adapter for `PeopleRepository`.

use async_trait::async_trait;

use crate::port::{PeopleError, PeopleRepository};
use crate::types::Employee;

pub struct InMemoryPeople {
    employees: std::sync::RwLock<Vec<Employee>>,
}

impl InMemoryPeople {
    pub fn new(employees: Vec<Employee>) -> Self {
        Self {
            employees: std::sync::RwLock::new(employees),
        }
    }
}

#[async_trait]
impl PeopleRepository for InMemoryPeople {
    async fn all_employees(&self) -> Result<Vec<Employee>, PeopleError> {
        Ok(self.employees.read().unwrap().clone())
    }

    async fn employee_by_id(&self, id: &str) -> Result<Option<Employee>, PeopleError> {
        Ok(self
            .employees
            .read()
            .unwrap()
            .iter()
            .find(|e| e.id == id)
            .cloned())
    }

    async fn direct_reports(&self, manager_id: &str) -> Result<Vec<Employee>, PeopleError> {
        Ok(self
            .employees
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.manager_id.as_deref() == Some(manager_id))
            .cloned()
            .collect())
    }

    async fn create_employee_at(
        &self,
        emp: &Employee,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<String, PeopleError> {
        let mut employees = self.employees.write().unwrap();
        if employees.iter().any(|e| e.id == emp.id) {
            return Err(PeopleError::Conflict(format!(
                "employee {} already exists",
                emp.id
            )));
        }
        let id = emp.id.clone();
        employees.push(emp.clone());
        Ok(id)
    }

    async fn update_employee_at(
        &self,
        id: &str,
        emp: &Employee,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), PeopleError> {
        let mut employees = self.employees.write().unwrap();
        let pos = employees
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| PeopleError::NotFound(id.to_string()))?;
        employees[pos] = emp.clone();
        Ok(())
    }

    async fn delete_employee(&self, id: &str) -> Result<(), PeopleError> {
        let mut employees = self.employees.write().unwrap();
        let pos = employees
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| PeopleError::NotFound(id.to_string()))?;
        employees.remove(pos);
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
