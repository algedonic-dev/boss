//! Wire + domain types for global search.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a search row points at. The three kinds the design names as
/// the unified result set — Subjects (identity), Jobs (work), events
/// (what happened) — which are three projections of one log rather
/// than three systems federated at the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Subject,
    Job,
    Event,
}

impl RefKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefKind::Subject => "subject",
            RefKind::Job => "job",
            RefKind::Event => "event",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "subject" => Some(RefKind::Subject),
            "job" => Some(RefKind::Job),
            "event" => Some(RefKind::Event),
            _ => None,
        }
    }
}

/// One indexed, findable thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRow {
    pub ref_kind: RefKind,
    pub ref_id: String,
    /// What this row is ABOUT. The join key that turns three result
    /// lists into one answer.
    pub subject_kind: Option<String>,
    pub subject_id: Option<String>,
    pub title: String,
    pub body: String,
    pub occurred_at: Option<DateTime<Utc>>,
}

/// A Subject hit, with the work about it and what happened to it.
///
/// This shape IS the claim. A federated search returns three lists that
/// mention the same customer; this returns the customer, the Jobs about
/// them, and the events behind those Jobs, joined on identity the
/// system issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectHit {
    pub subject_kind: String,
    pub subject_id: String,
    pub title: String,
    /// Jobs whose subject is this Subject.
    pub jobs: Vec<SearchRow>,
    /// Recent events about this Subject.
    pub events: Vec<SearchRow>,
    /// Total events indexed for this Subject — `events` is a preview.
    pub event_count: i64,
}

/// The full answer to one query.
///
/// Grouped by kind with a hard order rather than scored across kinds:
/// a relevance model that ranks an event above the account it happened
/// to is impossible to explain, and unexplainable ranking is how search
/// boxes lose trust. Recency only breaks ties WITHIN a kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchResults {
    pub query: String,
    /// Subjects, each carrying its own Jobs and events.
    pub subjects: Vec<SubjectHit>,
    /// Jobs that matched directly without their Subject matching.
    pub jobs: Vec<SearchRow>,
    /// Events that matched directly.
    pub events: Vec<SearchRow>,
}
