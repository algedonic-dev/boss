//! Shared test scaffolding for the people crate.
//!
//! Provides:
//! - `PeopleTestApp` builder wiring InMemoryPeople + HTTP router
//!   + RecordingEventBus + DomainPublisher
//! - `employee_fixture()` helper producing a valid Employee with sensible
//!   defaults for every required field
//!
//! Usage in a test:
//! ```ignore
//! let app = PeopleTestApp::new();
//! let emp = employee_fixture("emp-100");
//! let resp = TestRequest::post("/api/people").json(&emp).send(&app.router).await;
//! resp.assert_status(StatusCode::CREATED);
//! app.bus.assert_event_emitted("people.employee.created");
//! ```

#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use boss_people::InMemoryPeople;
use boss_people::http::{PeopleApiState, router};
use boss_people::types::*;
use boss_policy_client::{PermissivePolicyClient, PolicyClient};

/// A fully wired people service for tests (publisher `None` — outbox
/// phase 2: the adapter records events in the domain write, so the
/// in-memory repo's `recorded_events()` is the assertion surface).
pub struct PeopleTestApp {
    pub router: Router,
    pub people: Arc<InMemoryPeople>,
}

impl PeopleTestApp {
    /// Build a fresh test app with an empty roster.
    pub fn new() -> Self {
        Self::with_employees(vec![])
    }

    /// Build a test app pre-populated with the given employees.
    pub fn with_employees(employees: Vec<Employee>) -> Self {
        let people = Arc::new(InMemoryPeople::new(employees));
        let state = PeopleApiState {
            people: people.clone(),
            publisher: None,
            policy: Some(Arc::new(PermissivePolicyClient) as Arc<dyn PolicyClient>),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, people }
    }

    /// Assert exactly-one recorded event of `kind` and return it.
    pub fn assert_recorded(&self, kind: &str) -> boss_core::event::Event {
        let matches: Vec<_> = self
            .people
            .recorded_events()
            .into_iter()
            .filter(|e| e.kind == kind)
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one recorded `{kind}` event, got {}",
            matches.len()
        );
        matches.into_iter().next().unwrap()
    }

    /// Assert no recorded event of `kind`.
    pub fn assert_not_recorded(&self, kind: &str) {
        assert!(
            !self.people.recorded_events().iter().any(|e| e.kind == kind),
            "expected no recorded `{kind}` event"
        );
    }
}

/// Build a valid Employee suitable for create/update tests.
pub fn employee_fixture(id: &str) -> Employee {
    Employee {
        id: id.to_string(),
        name: Some(format!("Test Employee {id}")),
        email: Some(format!("{id}@boss.io")),
        role: Some("service-tech".to_string()),
        department: Some("service".to_string()),
        skill_level: Some(3),
        skills: vec![],
        hire_date: Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()),
        location: Some("loc-hq".to_string()),
        manager_id: None,
        employment_type: Some("full-time".to_string()),
        status: Some("active".to_string()),
        certifications: vec![],
        annual_salary_cents: None,
    }
}
