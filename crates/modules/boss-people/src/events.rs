//! Domain event subjects for people operations.
//!
//! Account events (`accounts.account.*`) live in
//! `boss_accounts::events`.

pub const EMPLOYEE_CREATED: &str = "people.employee.created";
pub const EMPLOYEE_UPDATED: &str = "people.employee.updated";
pub const EMPLOYEE_DELETED: &str = "people.employee.deleted";

/// Audit-trail row in `employee_changes`. Emitted alongside an
/// `EMPLOYEE_UPDATED` whenever the workflow surface mutates an
/// employee field that warrants a change log entry (status →
/// onboard/offboard/leave, role/department/salary deltas, etc.),
/// and standalone via the explicit `record_change` endpoint.
///
/// The payload shape mirrors `EmployeeChangeRecord`. The
/// rebuilder consumes this event by INSERTing into the
/// `employee_changes` projection so a `boss-rebuild-all` cycle
/// reproduces the historical change log from `audit_log` alone.
pub const EMPLOYEE_CHANGE_RECORDED: &str = "people.employee.change-recorded";

/// Hiring requisition opened. Payload mirrors the full
/// `Requisition` struct — every field needed to reproduce the
/// projection row. Status changes ride on the same event kind via
/// the ON CONFLICT DO UPDATE path the handler already uses.
pub const REQUISITION_OPENED: &str = "people.requisition.opened";
