//! Domain event subjects for the scheduling submodule.
//!
//! Every scheduling write emits one of these events. Each carries
//! full row state so the rebuilder can reproduce the projection
//! (tech_availability, scheduled_assignments, tech_shift_patterns,
//! tech_calendar_tokens) from `audit_log` alone.

/// Tech availability slot recorded (PTO, training, or manual hold).
/// Payload: full `TechAvailability` row.
pub const AVAILABILITY_CREATED: &str = "scheduling.availability.created";

/// Tech availability slot deleted. Payload `{id, deleted_at}`.
pub const AVAILABILITY_DELETED: &str = "scheduling.availability.deleted";

/// Scheduled assignment created (tech booked against a target job).
/// Payload: full `ScheduledAssignment` row.
pub const ASSIGNMENT_CREATED: &str = "scheduling.assignment.created";

/// Scheduled assignment deleted. Payload `{id, deleted_at}`.
pub const ASSIGNMENT_DELETED: &str = "scheduling.assignment.deleted";

/// Scheduled assignment status changed (tentative → confirmed →
/// completed → cancelled). Payload `{id, status, changed_at}`.
/// Rebuild updates the existing row's status column.
pub const ASSIGNMENT_STATUS_CHANGED: &str = "scheduling.assignment.status-changed";

/// Tech shift pattern upserted (weekly working-hours template).
/// Payload: full `TechShiftPattern` row.
pub const SHIFT_PATTERN_UPSERTED: &str = "scheduling.shift-pattern.upserted";

/// Calendar feed token rotated for a tech. Payload `{employee_id,
/// token, rotated_at}`. Rebuild UPSERTs the `tech_calendar_tokens`
/// row so a published `/ics/{token}/calendar.ics` URL keeps
/// resolving after replay.
pub const CALENDAR_TOKEN_ROTATED: &str = "scheduling.calendar-token.rotated";
