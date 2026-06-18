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
use boss_core::publisher::DomainPublisher;
#[cfg(feature = "postgres")]
use boss_events::PgAuditWriter;
use boss_people::InMemoryPeople;
use boss_people::http::{PeopleApiState, router};
use boss_people::types::*;
use boss_policy_client::{PermissivePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
#[cfg(feature = "postgres")]
use sqlx::PgPool;

/// A fully wired people service for tests.
pub struct PeopleTestApp {
    pub router: Router,
    pub bus: Arc<RecordingEventBus>,
}

impl PeopleTestApp {
    /// Build a fresh test app with an empty roster.
    pub fn new() -> Self {
        Self::with_employees(vec![])
    }

    /// Build a test app pre-populated with the given employees.
    pub fn with_employees(employees: Vec<Employee>) -> Self {
        let people = Arc::new(InMemoryPeople::new(employees));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "people");
        let state = PeopleApiState {
            people,
            publisher: Some(publisher),
            policy: Some(Arc::new(PermissivePolicyClient) as Arc<dyn PolicyClient>),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, bus }
    }

    /// Build a test app whose publisher persists every emitted event
    /// to the given Postgres pool's `audit_log` table. Used by the
    /// audit_log E2E integration test.
    #[cfg(feature = "postgres")]
    pub fn with_audit_pool(pool: PgPool) -> Self {
        let people = Arc::new(InMemoryPeople::new(vec![]));
        let bus = RecordingEventBus::new();
        let publisher = DomainPublisher::new(bus.clone(), "people")
            .with_audit(Arc::new(PgAuditWriter::new(pool)));
        let state = PeopleApiState {
            people,
            publisher: Some(publisher),
            policy: Some(Arc::new(PermissivePolicyClient) as Arc<dyn PolicyClient>),
            subject_kinds: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        let router = router(state);
        Self { router, bus }
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
