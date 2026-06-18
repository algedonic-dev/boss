//! Domain types for field-service scheduling. Mirror the shapes of
//! `tech_availability`, `scheduled_assignments`, `tech_shift_patterns`
//! in `infra/postgres/schema/28-scheduling.sql`.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AvailabilityKind {
    Available,
    Pto,
    Sick,
    Holiday,
    Training,
    Blocked,
}

impl AvailabilityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Pto => "pto",
            Self::Sick => "sick",
            Self::Holiday => "holiday",
            Self::Training => "training",
            Self::Blocked => "blocked",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "available" => Self::Available,
            "pto" => Self::Pto,
            "sick" => Self::Sick,
            "holiday" => Self::Holiday,
            "training" => Self::Training,
            "blocked" => Self::Blocked,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AvailabilitySource {
    Manual,
    ShiftPattern,
    Import,
}

impl AvailabilitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::ShiftPattern => "shift-pattern",
            Self::Import => "import",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "manual" => Self::Manual,
            "shift-pattern" => Self::ShiftPattern,
            "import" => Self::Import,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssignmentKind {
    Wo,
    #[serde(rename = "preventive-maintenance")]
    PreventiveMaintenance,
    Training,
    #[serde(rename = "diag-call")]
    DiagCall,
    Travel,
    Install,
}

impl AssignmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wo => "wo",
            Self::PreventiveMaintenance => "preventive-maintenance",
            Self::Training => "training",
            Self::DiagCall => "diag-call",
            Self::Travel => "travel",
            Self::Install => "install",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "wo" => Self::Wo,
            "preventive-maintenance" => Self::PreventiveMaintenance,
            "training" => Self::Training,
            "diag-call" => Self::DiagCall,
            "travel" => Self::Travel,
            "install" => Self::Install,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssignmentStatus {
    Tentative,
    Confirmed,
    Completed,
    Cancelled,
    NoShow,
}

impl AssignmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tentative => "tentative",
            Self::Confirmed => "confirmed",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::NoShow => "no-show",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "tentative" => Self::Tentative,
            "confirmed" => Self::Confirmed,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "no-show" => Self::NoShow,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Row shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechAvailability {
    pub id: Uuid,
    pub employee_id: String,
    pub kind: AvailabilityKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub notes: Option<String>,
    pub source: AvailabilitySource,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewTechAvailability {
    pub employee_id: String,
    pub kind: AvailabilityKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default = "default_source")]
    pub source: AvailabilitySource,
}
fn default_source() -> AvailabilitySource {
    AvailabilitySource::Manual
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledAssignment {
    pub id: Uuid,
    pub tech_id: String,
    pub target_job_id: Uuid,
    pub kind: AssignmentKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: AssignmentStatus,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewScheduledAssignment {
    pub tech_id: String,
    pub target_job_id: Uuid,
    pub kind: AssignmentKind,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    #[serde(default = "default_status")]
    pub status: AssignmentStatus,
    #[serde(default)]
    pub notes: Option<String>,
}
fn default_status() -> AssignmentStatus {
    AssignmentStatus::Tentative
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechShiftPattern {
    pub id: Uuid,
    pub employee_id: String,
    /// 0 = Sunday, 6 = Saturday, matching Postgres `EXTRACT(DOW FROM ...)`.
    pub day_of_week: i16,
    pub starts_at_time: NaiveTime,
    pub ends_at_time: NaiveTime,
    pub timezone: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Week-grid projection
// ---------------------------------------------------------------------------

/// One row in the week-grid projection — a tech plus their blocks
/// over the window. The frontend groups blocks into day columns.
#[derive(Debug, Clone, Serialize)]
pub struct WeekGridRow {
    pub employee_id: String,
    pub blocks: Vec<WeekGridBlock>,
}

/// A single block in the week grid. Encodes both availability
/// slots ("available 8-17 Mon") and scheduled assignments ("preventive maintenance
/// visit 10-12 Wed"). The frontend color-codes by kind + source.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum WeekGridBlock {
    Availability {
        id: Uuid,
        kind: AvailabilityKind,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
        notes: Option<String>,
    },
    Assignment {
        id: Uuid,
        kind: AssignmentKind,
        status: AssignmentStatus,
        target_job_id: Uuid,
        target_job_title: Option<String>,
        target_job_kind: Option<String>,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
        notes: Option<String>,
    },
}
