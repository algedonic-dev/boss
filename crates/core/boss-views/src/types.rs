//! Wire + domain types for Views.
//!
//! A **View** is a saved composition over the Information API. It is
//! deliberately NOT a gadget: it holds a query and a layout, never
//! records. Its content is computed from the same projections every
//! other surface reads, so two people running the same View see the
//! same numbers because there is only one set of numbers.
//!
//! See `docs/design/home-workspace-and-department-apps.md`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a View reads — the four foundational primitives.
///
/// Subjects (identity), Jobs (bounded work), Steps (the typed
/// transitions inside a Job), events (what happened). Steps was the
/// last one missing, which mattered more than the count suggests: a
/// Step's `status` is the program counter of the state machine, so
/// "what am I meant to be doing" is a question about Steps and could
/// not be asked.
///
/// Domain detail (an account's tier, a vendor's category) is
/// deliberately absent in this phase: Q1 of the global-search review
/// settled that core identity is served centrally and each app
/// contributes its own scoped search for its own fields. Views start
/// where that central answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewSource {
    Subjects,
    Jobs,
    Steps,
    Events,
}

impl ViewSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ViewSource::Subjects => "subjects",
            ViewSource::Jobs => "jobs",
            ViewSource::Steps => "steps",
            ViewSource::Events => "events",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "subjects" => Some(ViewSource::Subjects),
            "jobs" => Some(ViewSource::Jobs),
            "steps" => Some(ViewSource::Steps),
            "events" => Some(ViewSource::Events),
            _ => None,
        }
    }
}

/// How the rows are drawn. Small on purpose — a layout the author
/// cannot describe in one word is a report, and reports are a
/// different feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewLayout {
    Table,
    List,
    Count,
}

impl ViewLayout {
    pub fn as_str(&self) -> &'static str {
        match self {
            ViewLayout::Table => "table",
            ViewLayout::List => "list",
            ViewLayout::Count => "count",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "table" => Some(ViewLayout::Table),
            "list" => Some(ViewLayout::List),
            "count" => Some(ViewLayout::Count),
            _ => None,
        }
    }
}

/// Who can see a View.
///
/// Q4 of the review: sharing is free — no promotion Job stands
/// between an operator and showing a colleague something useful.
/// What needs a process is *inclusion in a department's views*, which
/// is a later phase and a different field; `Shared` here means
/// "visible to anyone who asks", not "adopted by a department".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Private,
    Shared,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Shared => "shared",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "private" => Some(Visibility::Private),
            "shared" => Some(Visibility::Shared),
            _ => None,
        }
    }
}

/// A saved View.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub id: String,
    /// The employee this View belongs to. Views are personal first;
    /// `visibility` is what widens them.
    pub owner_id: String,
    pub title: String,
    pub source: ViewSource,
    /// A `boss-expr` predicate evaluated against each candidate row.
    /// Empty means "no filter".
    ///
    /// Reusing the DSL that already backs dispatcher rule predicates
    /// and step `ready_when` rather than inventing a second one: the
    /// language cannot express non-termination, which is what makes
    /// running an operator-authored predicate over a result set safe
    /// without a sandbox. Q3's agent-written code is the phase that
    /// needs the sandbox; this phase deliberately does not.
    #[serde(default)]
    pub filter: String,
    /// Ordered field names to show. Empty means "the source's
    /// defaults".
    #[serde(default)]
    pub columns: Vec<String>,
    pub layout: ViewLayout,
    pub visibility: Visibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What a caller supplies to create or replace a View. Server owns
/// `id`, the timestamps, and — deliberately — `owner_id`.
///
/// `owner_id` is NOT on the wire. It was, and that made ownership a
/// caller-supplied string: anyone could POST a View attributed to
/// anyone. Deriving it from the authenticated caller removes the
/// spoof by construction rather than by validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewInput {
    pub title: String,
    pub source: ViewSource,
    #[serde(default)]
    pub filter: String,
    #[serde(default)]
    pub columns: Vec<String>,
    pub layout: ViewLayout,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
}

fn default_visibility() -> Visibility {
    Visibility::Private
}

/// The result of running a View.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewResults {
    pub view_id: String,
    pub source: ViewSource,
    pub layout: ViewLayout,
    /// Rows after filtering, capped at the request limit.
    pub rows: Vec<serde_json::Value>,
    /// How many rows matched the filter. For `count` layouts this is
    /// the whole answer.
    pub matched: usize,
    /// How many filter terms were answered by the database.
    ///
    /// Zero with a non-empty filter means nothing could be narrowed —
    /// the scan read the newest `SCAN_CEILING` rows and filtered them
    /// in-process, so a match older than that window is invisible.
    /// That combination is the least trustworthy answer this endpoint
    /// gives, and callers cannot tell it from a confident zero unless
    /// it is reported. `kind = "a" OR kind = "b"` is the everyday case:
    /// no term is pushable under OR, so a filter matching 16 old
    /// events reports 0.
    pub pushed_down: usize,
    /// True when the scan hit its ceiling before running out of
    /// candidate rows, so `matched` is a floor rather than a total.
    ///
    /// A View that silently truncates reads as a complete answer and
    /// is worse than one that admits it stopped early — an operator
    /// who cannot tell the difference will act on the wrong number.
    pub truncated: bool,
}
