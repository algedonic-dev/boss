//! Postgres adapter for `PeopleRepository`.
//!
//! Queries `employees`, `employee_skills`, and `employee_certifications`
//! tables and assembles into `Employee` structs.

use std::sync::Arc;

use async_trait::async_trait;
use boss_classes_client::ClassesClient;
use boss_core::primitives::ClassRef;
use boss_locations_client::LocationsClient;
use sqlx::PgPool;

use crate::departments::{DepartmentRoster, PgDepartmentRoster, validate_department};
use crate::port::{PeopleError, PeopleRepository, refuse_another_id, refuse_malformed};
use crate::types::*;

pub struct PgPeople {
    pool: PgPool,
    /// Optional Class registry client. When present, every write
    /// validates the closed-set attributes of `Employee` (`role`,
    /// `employment_type`, `status`) against
    /// `class_exists_on(("employee", code), column)` before the row
    /// hits the DB — on the column's own axis, not merely the kind.
    /// `skill_level` is a numeric range (1..=5), not a closed enum,
    /// so it stays on its CHECK and is not a Class registry
    /// candidate.
    classes: Option<Arc<dyn ClassesClient>>,
    /// Optional Locations registry client. When present, every write
    /// validates `location` against `location_exists(id)`.
    locations: Option<Arc<dyn LocationsClient>>,
    /// Optional departments registry. When present, every write
    /// validates `department` against the un-retired `departments`
    /// rows — the registry `GET /api/departments` serves — and not the
    /// `employee` department Classes it was checked against until
    /// backlog c87e3d6d (see `crate::departments`).
    departments: Option<Arc<dyn DepartmentRoster>>,
}

impl PgPeople {
    /// Construct a PgPeople with no registry clients wired. Used by
    /// in-memory / test paths; production binaries always wire both
    /// clients via `with_registries`.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            classes: None,
            locations: None,
            departments: None,
        }
    }

    /// Construct a PgPeople wired to its registries. Every write
    /// validates the closed-set Class attributes (`role`,
    /// `employment_type`, `status`), the `department` against the
    /// departments registry — read on this same pool, the table being
    /// in the database the service already holds — and the Location
    /// id before committing.
    pub fn with_registries(
        pool: PgPool,
        classes: Arc<dyn ClassesClient>,
        locations: Arc<dyn LocationsClient>,
    ) -> Self {
        let departments: Arc<dyn DepartmentRoster> =
            Arc::new(PgDepartmentRoster::new(pool.clone()));
        Self {
            pool,
            classes: Some(classes),
            locations: Some(locations),
            departments: Some(departments),
        }
    }

    /// Reject writes whose `role` doesn't resolve to an active Class.
    /// No-op when no `classes` client is configured.
    async fn validate_role(&self, role_code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("role", role_code).await
    }

    /// Reject writes whose `department` is not an active department in
    /// the departments registry. No-op when no registry is configured.
    async fn validate_department(&self, department_code: &str) -> Result<(), PeopleError> {
        let Some(departments) = &self.departments else {
            return Ok(());
        };
        validate_department(departments.as_ref(), department_code).await
    }

    /// Reject writes whose `employment_type` doesn't resolve to an
    /// active Class. No-op when no `classes` client is configured.
    async fn validate_employment_type(&self, code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("employment_type", code).await
    }

    /// Reject writes whose `status` doesn't resolve to an active
    /// Class. No-op when no `classes` client is configured.
    async fn validate_status(&self, code: &str) -> Result<(), PeopleError> {
        self.validate_employee_class("status", code).await
    }

    async fn validate_employee_class(
        &self,
        attribute: &str,
        code: &str,
    ) -> Result<(), PeopleError> {
        let Some(classes) = &self.classes else {
            return Ok(());
        };
        validate_employee_class(classes.as_ref(), attribute, code).await
    }

    /// Reject writes whose `location` doesn't resolve to an active
    /// Location id in the registry. No-op when no `locations` client
    /// is configured.
    async fn validate_location(&self, location_id: &str) -> Result<(), PeopleError> {
        let Some(locations) = &self.locations else {
            return Ok(());
        };
        let exists = locations
            .location_exists(location_id)
            .await
            .map_err(|e| PeopleError::Storage(format!("locations registry: {e}")))?;
        if !exists {
            return Err(PeopleError::Conflict(format!(
                "location `{location_id}` is not an active Location in the registry"
            )));
        }
        Ok(())
    }
}

/// Reject a write whose `attribute` column names a code that is not an
/// active `employee` Class ON THAT AXIS.
///
/// The `employee` drawer holds four taxonomies under one subject_kind —
/// role, department, status, employment_type, 22 live codes on
/// 2026-09-23 (backlog a45ab09d) — and the column name IS the Class's
/// `member_attribute` (the column on the Subject whose value equals the
/// code). Until that day this asked only whether `(employee, code)`
/// existed, so `role = "terminated"` and `department = "platform-admin"`
/// both passed. Asking on the axis is what keeps the four apart while
/// they still share a kind. Three since backlog c87e3d6d (2026-09-27):
/// `department` left the drawer for the departments registry
/// (`crate::departments`).
async fn validate_employee_class(
    classes: &dyn ClassesClient,
    attribute: &str,
    code: &str,
) -> Result<(), PeopleError> {
    let class_ref = ClassRef::new("employee", code);
    let exists = classes
        .class_exists_on(&class_ref, attribute)
        .await
        .map_err(|e| {
            // Upstream service failure — surface as Storage so the
            // caller's retry/error UX matches a DB hiccup.
            PeopleError::Storage(format!("classes registry: {e}"))
        })?;
    if !exists {
        return Err(PeopleError::Conflict(format!(
            "{attribute} `{code}` is not an active `{attribute}` Class in the registry"
        )));
    }
    Ok(())
}

#[async_trait]
impl PeopleRepository for PgPeople {
    async fn all_employees(&self) -> Result<Vec<Employee>, PeopleError> {
        let rows: Vec<EmployeeRow> = // Byte order, as the port states it and the double answers
        // (backlog be459ab9; the locale put `suite-ab` before `suite-B`).
        sqlx::query_as("SELECT * FROM employees ORDER BY id COLLATE \"C\"")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;

        let mut employees = Vec::with_capacity(rows.len());
        for row in rows {
            let emp = self.assemble(row).await?;
            employees.push(emp);
        }
        Ok(employees)
    }

    async fn employee_by_id(&self, id: &str) -> Result<Option<Employee>, PeopleError> {
        let row: Option<EmployeeRow> = sqlx::query_as("SELECT * FROM employees WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(self.assemble(r).await?)),
            None => Ok(None),
        }
    }

    async fn create_employee_at(
        &self,
        emp: &Employee,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<String, PeopleError> {
        // Registry validation runs before the transaction so a
        // mis-typed code doesn't waste a Postgres connection.
        // Identity-first: validate only the descriptive fields that are
        // present. An id-only employee record carries none of these yet;
        // each is validated against its Class registry once assigned.
        if let Some(role) = &emp.role {
            self.validate_role(role).await?;
        }
        if let Some(department) = &emp.department {
            self.validate_department(department).await?;
        }
        if let Some(employment_type) = &emp.employment_type {
            self.validate_employment_type(&to_kebab(employment_type))
                .await?;
        }
        if let Some(status) = &emp.status {
            self.validate_status(&to_kebab(status)).await?;
        }
        if let Some(location) = &emp.location {
            self.validate_location(location).await?;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM employees WHERE id = $1)")
                .bind(&emp.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| PeopleError::Storage(e.to_string()))?;
        // Identity write-through (subject-model R1, Q1): same tx as
        // the domain row.
        boss_subject_kinds::subjects::record_subject_in_tx(
            &mut tx,
            "employee",
            &emp.id,
            emp.name.as_deref(),
        )
        .await
        .map_err(PeopleError::Storage)?;
        if exists {
            return Err(PeopleError::Conflict(format!(
                "employee {} already exists",
                emp.id
            )));
        }
        refuse_unholdable(&mut tx, emp).await?;
        upsert_employee_row(&mut tx, emp, now).await?;
        insert_employee_satellites(&mut tx, emp).await?;
        // OUTBOX (phase 2): the created event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::EMPLOYEE_CREATED,
            serde_json::to_value(emp).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(PeopleError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        Ok(emp.id.clone())
    }

    async fn update_employee_at(
        &self,
        id: &str,
        emp: &Employee,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), PeopleError> {
        // Identity-first: validate only the descriptive fields that are
        // present. An id-only employee record carries none of these yet;
        // each is validated against its Class registry once assigned.
        if let Some(role) = &emp.role {
            self.validate_role(role).await?;
        }
        if let Some(department) = &emp.department {
            self.validate_department(department).await?;
        }
        if let Some(employment_type) = &emp.employment_type {
            self.validate_employment_type(&to_kebab(employment_type))
                .await?;
        }
        if let Some(status) = &emp.status {
            self.validate_status(&to_kebab(status)).await?;
        }
        if let Some(location) = &emp.location {
            self.validate_location(location).await?;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM employees WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| PeopleError::Storage(e.to_string()))?;
        if !exists {
            return Err(PeopleError::NotFound(id.to_string()));
        }
        refuse_another_id(id, emp)?;
        refuse_unholdable(&mut tx, emp).await?;
        // UPSERT preserves `created_at` (load-bearing for rebuild
        // equality). Satellites still get full replacement since
        // skills + certifications have no per-row id we can UPSERT
        // by — drift would accumulate otherwise.
        upsert_employee_row(&mut tx, emp, now).await?;
        sqlx::query("DELETE FROM employee_skills WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM employee_certifications WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        insert_employee_satellites(&mut tx, emp).await?;
        // OUTBOX (phase 2): the updated event (full row state)
        // records with the rows.
        let event = stamp.event(
            crate::events::EMPLOYEE_UPDATED,
            serde_json::to_value(emp).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(PeopleError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_employee_at(
        &self,
        id: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), PeopleError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        // A manager with a report is refused by name, as the double
        // refuses it — the foreign key answered a `Storage` error, a
        // 500, until backlog be459ab9.
        let report: Option<String> = sqlx::query_scalar(
            "SELECT id FROM employees WHERE manager_id = $1 AND id <> $1 \
             ORDER BY id COLLATE \"C\" LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;
        if let Some(report) = report {
            return Err(PeopleError::Conflict(format!(
                "employee {id} still manages {report}"
            )));
        }
        sqlx::query("DELETE FROM employee_skills WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        sqlx::query("DELETE FROM employee_certifications WHERE employee_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        let result = sqlx::query("DELETE FROM employees WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(PeopleError::NotFound(id.to_string()));
        }
        // OUTBOX (phase 2): the deleted event records only after the
        // row actually deleted (NotFound above returns pre-recording).
        let event = stamp.event(
            crate::events::EMPLOYEE_DELETED,
            serde_json::json!({ "id": id, "deleted_at": now }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(PeopleError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| PeopleError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn direct_reports(&self, manager_id: &str) -> Result<Vec<Employee>, PeopleError> {
        let rows: Vec<EmployeeRow> = sqlx::query_as(
            // Byte order, nameless last, id breaking ties — the port's
            // words (backlog be459ab9; this was the locale's order of
            // `name` alone).
            "SELECT * FROM employees WHERE manager_id = $1 \
                 ORDER BY name COLLATE \"C\" NULLS LAST, id COLLATE \"C\"",
        )
        .bind(manager_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;

        let mut employees = Vec::with_capacity(rows.len());
        for row in rows {
            let emp = self.assemble(row).await?;
            employees.push(emp);
        }
        Ok(employees)
    }
}

impl PgPeople {
    async fn assemble(&self, row: EmployeeRow) -> Result<Employee, PeopleError> {
        let (skills, certifications) = tokio::try_join!(
            self.fetch_skills(&row.id),
            self.fetch_certifications(&row.id),
        )?;

        Ok(row.into_employee(skills, certifications))
    }

    async fn fetch_skills(&self, employee_id: &str) -> Result<Vec<String>, PeopleError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT skill FROM employee_skills WHERE employee_id = $1 ORDER BY skill COLLATE \"C\"",
        )
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;

        Ok(rows.into_iter().map(|(s,)| s).collect())
    }

    async fn fetch_certifications(
        &self,
        employee_id: &str,
    ) -> Result<Vec<Certification>, PeopleError> {
        let rows: Vec<CertRow> = sqlx::query_as(
            "SELECT name, issuing_body, issued_on, expires_on FROM employee_certifications WHERE employee_id = $1 \
             ORDER BY issued_on, name COLLATE \"C\", id",
        )
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| PeopleError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| Certification {
                name: r.name,
                issuing_body: r.issuing_body,
                issued_on: r.issued_on,
                expires_on: r.expires_on,
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Write helpers
// ---------------------------------------------------------------------------

pub(crate) fn to_kebab<T: serde::Serialize>(val: &T) -> String {
    serde_json::to_value(val)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// The roster-wide refusals the port states, asked inside the write's
/// own transaction so a `Conflict` names the value instead of the
/// constraint answering a `Storage` error (the email unique index, the
/// `manager_id` foreign key, the skill-level CHECK, the skills primary
/// key) — what the double refuses, in the same words (backlog
/// be459ab9).
async fn refuse_unholdable(tx: &mut sqlx::PgConnection, e: &Employee) -> Result<(), PeopleError> {
    refuse_malformed(e)?;
    if let Some(email) = &e.email {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM employees WHERE LOWER(email) = LOWER($1) AND id <> $2)",
        )
        .bind(email)
        .bind(&e.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| PeopleError::Storage(err.to_string()))?;
        if taken {
            return Err(PeopleError::Conflict(format!(
                "email `{email}` is held by another employee"
            )));
        }
    }
    if let Some(manager) = e.manager_id.as_ref().filter(|m| **m != e.id) {
        let held: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM employees WHERE id = $1)")
            .bind(manager)
            .fetch_one(&mut *tx)
            .await
            .map_err(|err| PeopleError::Storage(err.to_string()))?;
        if !held {
            return Err(PeopleError::Conflict(format!(
                "manager `{manager}` is not an employee"
            )));
        }
    }
    Ok(())
}

pub(crate) async fn upsert_employee_row(
    tx: &mut sqlx::PgConnection,
    e: &Employee,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), PeopleError> {
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, skill_level, \
         hire_date, location, manager_id, employment_type, status, annual_salary_cents, \
         created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$13) \
         ON CONFLICT (id) DO UPDATE SET \
            name = EXCLUDED.name, \
            email = EXCLUDED.email, \
            role = EXCLUDED.role, \
            department = EXCLUDED.department, \
            skill_level = EXCLUDED.skill_level, \
            hire_date = EXCLUDED.hire_date, \
            location = EXCLUDED.location, \
            manager_id = EXCLUDED.manager_id, \
            employment_type = EXCLUDED.employment_type, \
            status = EXCLUDED.status, \
            annual_salary_cents = EXCLUDED.annual_salary_cents, \
            updated_at = EXCLUDED.updated_at",
    )
    .bind(&e.id)
    .bind(&e.name)
    .bind(&e.email)
    .bind(&e.role)
    .bind(&e.department)
    .bind(e.skill_level.map(|v| v as i16))
    .bind(e.hire_date)
    .bind(&e.location)
    .bind(&e.manager_id)
    .bind(e.employment_type.as_ref().map(to_kebab))
    .bind(e.status.as_ref().map(to_kebab))
    .bind(e.annual_salary_cents)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| PeopleError::Storage(e.to_string()))?;
    Ok(())
}

pub(crate) async fn insert_employee_satellites(
    tx: &mut sqlx::PgConnection,
    e: &Employee,
) -> Result<(), PeopleError> {
    for skill in &e.skills {
        sqlx::query("INSERT INTO employee_skills (employee_id, skill) VALUES ($1, $2)")
            .bind(&e.id)
            .bind(skill)
            .execute(&mut *tx)
            .await
            .map_err(|err| PeopleError::Storage(err.to_string()))?;
    }
    for cert in &e.certifications {
        sqlx::query(
            "INSERT INTO employee_certifications (employee_id, name, issuing_body, issued_on, expires_on) \
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(&e.id)
        .bind(&cert.name)
        .bind(&cert.issuing_body)
        .bind(cert.issued_on)
        .bind(cert.expires_on)
        .execute(&mut *tx)
        .await
        .map_err(|err| PeopleError::Storage(err.to_string()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct EmployeeRow {
    id: String,
    // Identity-first: descriptive columns are nullable (see Employee).
    name: Option<String>,
    email: Option<String>,
    role: Option<String>,
    department: Option<String>,
    skill_level: Option<i16>,
    hire_date: Option<chrono::NaiveDate>,
    location: Option<String>,
    manager_id: Option<String>,
    employment_type: Option<String>,
    status: Option<String>,
    annual_salary_cents: Option<i64>,
    // No `created_at` / `updated_at`: `Employee` carries neither, and
    // `FromRow` ignores columns it has no field for.
}

impl EmployeeRow {
    fn into_employee(self, skills: Vec<String>, certifications: Vec<Certification>) -> Employee {
        Employee {
            id: self.id,
            name: self.name,
            email: self.email,
            role: self.role,
            department: self.department,
            skill_level: self.skill_level.map(|v| v as u8),
            skills,
            hire_date: self.hire_date,
            location: self.location,
            manager_id: self.manager_id,
            // Trust the column — write-time validation against the
            // Class registry catches invalid values before they land,
            // so the column is just a string here.
            employment_type: self.employment_type,
            status: self.status,
            certifications,
            annual_salary_cents: self.annual_salary_cents,
        }
    }
}

#[derive(sqlx::FromRow)]
struct CertRow {
    name: String,
    issuing_body: String,
    issued_on: chrono::NaiveDate,
    expires_on: Option<chrono::NaiveDate>,
}

// Employee taxonomies (employment_type, status, …) carry no Rust-side
// closed set: every value is validated at write time against the Class
// registry (see validate_employee_class).

#[cfg(test)]
mod tests {
    use super::*;
    use boss_classes_client::FakeClassesClient;
    use boss_core::primitives::Class;

    /// One row per axis the live `employee` drawer carries (22 codes
    /// across role, department, status and employment_type, read from
    /// `GET /api/classes?subject_kind=employee` on 2026-09-23 — backlog
    /// a45ab09d), enough to ask each column about another's code. The
    /// department axis is gone from the check (c87e3d6d) — its column
    /// asks the departments registry — but a drawer still holding a
    /// department row must not lend it to another column.
    fn drawer() -> FakeClassesClient {
        let row = |code: &str, attribute: &str| Class {
            subject_kind: "employee".into(),
            code: code.into(),
            display_name: code.into(),
            parent_code: None,
            member_attribute: Some(attribute.into()),
            metadata: serde_json::Value::Null,
            sort_order: 0,
            retired_at: None,
        };
        FakeClassesClient::with_classes(vec![
            row("platform-admin", "role"),
            row("it", "department"),
            row("terminated", "status"),
            row("contractor", "employment_type"),
        ])
    }

    #[tokio::test]
    async fn each_column_accepts_the_codes_of_its_own_axis() {
        let classes = drawer();
        for (attribute, code) in [
            ("role", "platform-admin"),
            ("status", "terminated"),
            ("employment_type", "contractor"),
        ] {
            assert!(
                validate_employee_class(&classes, attribute, code)
                    .await
                    .is_ok(),
                "{attribute} `{code}` is on its own axis"
            );
        }
    }

    /// The defect: until 2026-09-23 the check asked only whether
    /// `(employee, code)` existed, so a status passed as a role and a
    /// role as a department.
    #[tokio::test]
    async fn a_column_refuses_another_axis_code_by_name() {
        let classes = drawer();
        for (attribute, code) in [
            ("role", "terminated"),
            ("role", "it"),
            ("status", "contractor"),
            ("employment_type", "it"),
        ] {
            match validate_employee_class(&classes, attribute, code).await {
                Err(PeopleError::Conflict(msg)) => assert!(
                    msg.contains(attribute) && msg.contains(code),
                    "the refusal names the column and the code: {msg}"
                ),
                other => panic!("{attribute} `{code}` must be refused, got {other:?}"),
            }
        }
    }
}
