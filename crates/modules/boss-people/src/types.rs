//! People domain types — employees, org chart, certifications, requisitions.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

pub type EmployeeId = String;

// Every employee taxonomy (role, department, status, employment_type)
// is a Class of `employee` Subjects, keyed
// `(subject_kind='employee', member_attribute=<attr>)` and validated
// on write via boss-classes-client. Keeping them as data lets a tenant
// add e.g. 'seasonal' or 'sabbatical' without forking core — the
// fields below are plain `String`.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Certification {
    pub name: String,
    pub issuing_body: String,
    pub issued_on: NaiveDate,
    pub expires_on: Option<NaiveDate>,
}

impl From<&Certification> for boss_core::primitives::Part {
    /// Certifications are an AttributePart of their parent Employee
    /// Subject. The full row serialises into the attribute value so
    /// the KB view can render every field without a second lookup.
    fn from(c: &Certification) -> Self {
        boss_core::primitives::Part::attribute(
            "certification",
            serde_json::to_value(c).expect("Certification serialises"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Employee {
    pub id: EmployeeId,
    /// Identity-first: only `id` is required to create an employee
    /// (e.g. a record opened at offer-acceptance before HR details are
    /// finalized). Every descriptive field below is nullable and
    /// enriched as onboarding proceeds — a record with `role`/`status`
    /// still `None` is simply not yet operational (the workforce won't
    /// assign it work, payroll won't pay it) until enriched.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    /// Class registry code under
    /// `(subject_kind='employee', member_attribute='role')`.
    /// Validated on writes via the boss-classes-client. `None` until
    /// assigned.
    #[serde(default)]
    pub role: Option<String>,
    /// Class registry code under
    /// `(subject_kind='employee', member_attribute='department')`.
    /// Validated on writes via the boss-classes-client. `None` until assigned.
    #[serde(default)]
    pub department: Option<String>,
    pub skill_level: Option<u8>,
    pub skills: Vec<String>,
    #[serde(default)]
    pub hire_date: Option<NaiveDate>,
    /// Location id (FK to `locations(id)`). Validated against the
    /// Locations registry on writes. `None` until assigned.
    #[serde(default)]
    pub location: Option<String>,
    pub manager_id: Option<EmployeeId>,
    /// Class registry code under
    /// `(subject_kind='employee', member_attribute='employment_type')`.
    /// Validated on writes via the boss-classes-client. `None` until
    /// assigned.
    #[serde(default)]
    pub employment_type: Option<String>,
    /// Class registry code under
    /// `(subject_kind='employee', member_attribute='status')`.
    /// Validated on writes via the boss-classes-client. `None` until
    /// onboarded.
    #[serde(default)]
    pub status: Option<String>,
    pub certifications: Vec<Certification>,
    /// Annual gross compensation in cents. Read by the sim's payroll
    /// path (docs/architecture-decisions.md §Simulator). Optional: the
    /// Pg adapter binds NULL when this is `None`, so salary can be set
    /// out of band.
    #[serde(default)]
    pub annual_salary_cents: Option<i64>,
}

#[cfg(test)]
mod part_conversion_tests {
    use super::*;
    use boss_core::primitives::Part;

    #[test]
    fn certification_converts_to_attribute_part() {
        let cert = Certification {
            name: "Erbium glass operator".into(),
            issuing_body: "FDA".into(),
            issued_on: NaiveDate::from_ymd_opt(2025, 1, 15).unwrap(),
            expires_on: Some(NaiveDate::from_ymd_opt(2028, 1, 15).unwrap()),
        };
        let part: Part = (&cert).into();
        match part {
            Part::Attribute { key, value } => {
                assert_eq!(key, "certification");
                assert_eq!(
                    value.get("name").and_then(|v| v.as_str()),
                    Some("Erbium glass operator"),
                );
                assert_eq!(
                    value.get("issuing_body").and_then(|v| v.as_str()),
                    Some("FDA"),
                );
            }
            Part::Subject { .. } => panic!("certification should be AttributePart"),
        }
    }
}
