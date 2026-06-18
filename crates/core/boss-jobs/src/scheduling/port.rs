//! Scheduling port — the trait the frontend's surfaces and the
//! materialize binary call into.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use super::types::{
    NewScheduledAssignment, NewTechAvailability, ScheduledAssignment, TechAvailability,
    TechShiftPattern, WeekGridRow,
};

#[derive(Debug, thiserror::Error)]
pub enum SchedulingError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("storage: {0}")]
    Storage(String),
}

#[async_trait]
pub trait SchedulingRepository: Send + Sync {
    // ----- Availability -----
    async fn create_availability(
        &self,
        new: NewTechAvailability,
    ) -> Result<TechAvailability, SchedulingError>;
    async fn list_availability(
        &self,
        employee_id: Option<&str>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError>;
    async fn delete_availability(&self, id: Uuid) -> Result<(), SchedulingError>;

    // ----- Assignments -----
    async fn create_assignment(
        &self,
        new: NewScheduledAssignment,
    ) -> Result<ScheduledAssignment, SchedulingError>;
    async fn get_assignment(
        &self,
        id: Uuid,
    ) -> Result<Option<ScheduledAssignment>, SchedulingError>;
    async fn list_assignments(
        &self,
        tech_id: Option<&str>,
        target_job_id: Option<Uuid>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ScheduledAssignment>, SchedulingError>;
    async fn update_assignment_status(
        &self,
        id: Uuid,
        status: super::types::AssignmentStatus,
    ) -> Result<(), SchedulingError>;
    async fn delete_assignment(&self, id: Uuid) -> Result<(), SchedulingError>;

    // ----- Shift patterns -----
    async fn upsert_shift_pattern(
        &self,
        employee_id: &str,
        day_of_week: i16,
        starts_at_time: chrono::NaiveTime,
        ends_at_time: chrono::NaiveTime,
        timezone: &str,
        effective_from: NaiveDate,
    ) -> Result<TechShiftPattern, SchedulingError>;
    async fn list_shift_patterns(
        &self,
        employee_id: Option<&str>,
    ) -> Result<Vec<TechShiftPattern>, SchedulingError>;

    /// Materializes `tech_availability` rows from each active
    /// `tech_shift_pattern` for the date window `[from, to)`.
    /// Idempotent — skips rows that already exist from the same source
    /// (via source='shift-pattern' + overlapping time range).
    /// Returns the number of rows inserted.
    async fn materialize_shift_patterns(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<i64, SchedulingError>;

    // ----- Projections -----
    /// Returns per-employee blocks over [from, to). Employees with no
    /// availability AND no assignments are omitted.
    async fn week_grid(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        employee_ids: Option<&[String]>,
    ) -> Result<Vec<WeekGridRow>, SchedulingError>;

    // ----- ICS calendar feed -----
    /// Returns the tech's current calendar-feed token, or `None` if
    /// they've never requested one.
    async fn calendar_token_for(
        &self,
        employee_id: &str,
    ) -> Result<Option<String>, SchedulingError>;

    /// Writes `new_token` as the tech's single active token, replacing
    /// whatever was there. Rotating invalidates the old URL.
    async fn rotate_calendar_token(
        &self,
        employee_id: &str,
        new_token: &str,
    ) -> Result<(), SchedulingError>;

    /// Reverse lookup for the public `/ics/{token}.ics` endpoint.
    /// `None` means the token doesn't match anyone.
    async fn employee_by_calendar_token(
        &self,
        token: &str,
    ) -> Result<Option<String>, SchedulingError>;
}
