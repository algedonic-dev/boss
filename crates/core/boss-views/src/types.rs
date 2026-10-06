//! Wire + domain types for Views.
//!
//! A **View** is a saved composition over the Information API. It is
//! deliberately NOT a gadget: it holds a query and a layout, never
//! records. Its content is computed from the same projections every
//! other surface reads, scoped to whoever runs it — so two people
//! running one View see the same rows only if their policy grants the
//! same rows, and `ViewResults::scope` says which case a result is.
//!
//! See `docs/architecture-decisions.md` §Step UX & frontend (the
//! folded home of the Views / department-apps decisions).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::ViewsError;

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
    /// Every source, in the order the composer offers them.
    pub const ALL: [ViewSource; 4] = [
        ViewSource::Jobs,
        ViewSource::Steps,
        ViewSource::Subjects,
        ViewSource::Events,
    ];

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

/// Whether a text value can be stored at all: Postgres TEXT cannot hold
/// a NUL byte.
pub(crate) fn storable(s: &str) -> bool {
    !s.contains('\0')
}

/// Refuse a write whose text cannot be stored, naming the field.
///
/// WHY IT EXISTS (backlog be459ab9, found by the adapters-agree suite,
/// 2026-09-30). Postgres refused a NUL byte in any of these with its
/// encoding error as `Storage` text — a 500 carrying database prose —
/// while the in-memory adapter stored it and answered. Both adapters
/// now ask this before they write, so both refuse the same way and
/// neither writes. A create also asks it of the owner it stamps; a
/// replace's owner is a lookup key, never written, so a NUL there is
/// the miss it is (`NotFound`).
pub(crate) fn refuse_unstorable(
    owner_id: Option<&str>,
    input: &ViewInput,
) -> Result<(), ViewsError> {
    let nul = |field: &str| ViewsError::Invalid(format!("{field} holds a NUL byte"));
    if !owner_id.is_none_or(storable) {
        return Err(nul("owner_id"));
    }
    if !storable(&input.title) {
        return Err(nul("title"));
    }
    if !storable(&input.filter) {
        return Err(nul("filter"));
    }
    if !input.columns.iter().all(|c| storable(c)) {
        return Err(nul("columns"));
    }
    Ok(())
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
    /// Whose rows these are: every row of the source, or only those
    /// the caller's policy lets them read (backlog 5392cf23).
    ///
    /// Scope is the CALLER's, so a shared View run by a narrower role
    /// shows that role its own rows — two people running one View can
    /// see different numbers, and this is the field that says so. A
    /// caller who may read none of the source is refused (403), never
    /// answered with zero rows.
    pub scope: ResultScope,
}

/// Which rows of a source a result was drawn from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultScope {
    /// Every row of the source.
    All,
    /// Only rows the caller owns or is assigned — the policy grant was
    /// narrower than the source.
    Owners,
}

/// What each source offers a View author, served by
/// `GET /api/views/sources` (backlog 4a8939b5).
///
/// The page used to keep its own copies of all three facts — the
/// fields, the pushable names, the ceiling — "in step with query.rs by
/// hand", and two had drifted. Served from the resolver's own lists,
/// they cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSources {
    /// How many candidate rows one View may scan (`query::SCAN_CEILING`).
    pub scan_ceiling: i64,
    pub sources: Vec<SourceSchema>,
}

/// One source's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSchema {
    pub source: ViewSource,
    /// Every field a row of this source carries, in the order the
    /// column picker offers them.
    pub fields: Vec<String>,
    /// The fields a filter term on which is answered by the database
    /// rather than over the newest rows only.
    pub pushable: Vec<PushableField>,
}

/// A field a filter can push into SQL, and how: `text` by equality or
/// set, `timestamp` by a range, `json` through a dotted path
/// (`payload.sku`, `metadata.department`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushableField {
    pub field: String,
    #[serde(rename = "type")]
    pub column_type: crate::pushdown::ColumnType,
}
