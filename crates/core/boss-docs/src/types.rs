//! Domain types for the design decision tracker.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status lifecycle for a design doc. Parsed from the `**Status**:`
/// line in the markdown file.
///
/// `Approved` and `Shipped` are distinct so the tracker can hide
/// shipped work (the implementation landed) while keeping approved-
/// but-unimplemented docs visible — those are exactly the ones a
/// reader coming to the tracker wants to pick up.
///
/// `Reopened` is the explicit "this was shipped but a new proposal
/// extending it is open for review" state. Parser validation rejects
/// a Shipped/Superseded doc that still has unresolved open questions
/// — the author must either resolve the questions or transition the
/// doc to Reopened so the tracker surfaces the new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocStatus {
    Draft,
    InReview,
    Approved,
    Shipped,
    Reopened,
    Superseded,
    /// A settled reference the codebase lives by — a reading frame,
    /// contract, or governance rule that is NOT being acted upon.
    /// Distinct from `InReview` (open questions, active discussion)
    /// and from `Shipped` (a proposal whose work completed): living
    /// docs stay current indefinitely and carry no open questions —
    /// growing a `### Qn:` means flipping to `reopened` first.
    Living,
}

impl DocStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::InReview => "in-review",
            Self::Approved => "approved",
            Self::Shipped => "shipped",
            Self::Reopened => "reopened",
            Self::Superseded => "superseded",
            Self::Living => "living",
        }
    }

    /// Parse from a `**Status**:` line value.
    ///
    /// **Leading-token first.** Reads the first word (before the
    /// first separator — `—`, `-`, `,`, `.`, parenthesis, or
    /// whitespace) and matches it against the known status
    /// vocabulary. This is the only safe parse for prose like
    /// "living — HumanWorker engine shipped, BatchEngine in
    /// design": the descriptive tail mentions `shipped` but the
    /// status itself is `living`, and a `contains("shipped")`
    /// substring match misclassifies the whole doc.
    ///
    /// Falls back to a substring scan only when the leading
    /// token doesn't match — preserves backward compatibility
    /// for older free-prose statuses.
    ///
    /// Vocabulary:
    /// - `superseded` → Superseded
    /// - `reopened` → Reopened (wins over `shipped` so a doc can
    ///   say "reopened — original scope shipped" without
    ///   misclassifying)
    /// - `shipped` / `done` / `complete` → Shipped
    /// - `approved` / `accepted` → Approved
    /// - `draft` → Draft
    /// - `living` / `stable` / `load-bearing` → Living (settled
    ///   references — reading frames, contracts, governance rules
    ///   that are not being acted upon; pre-2026-07-08 these
    ///   collapsed into InReview and every reference doc read as if
    ///   it were under active discussion)
    /// - `in-review` / `open` → InReview
    /// - else (no recognized leading token) → substring
    ///   fallback below
    pub fn from_status_line(value: &str) -> Self {
        let leading: String = value
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
            .to_lowercase();
        if let Some(s) = match leading.as_str() {
            "superseded" => Some(Self::Superseded),
            "reopened" => Some(Self::Reopened),
            "shipped" | "done" | "complete" => Some(Self::Shipped),
            "approved" | "accepted" => Some(Self::Approved),
            "draft" => Some(Self::Draft),
            "living" | "stable" | "load-bearing" => Some(Self::Living),
            "in-review" | "open" => Some(Self::InReview),
            _ => None,
        } {
            return s;
        }
        // Substring fallback. Order matters: `reopened` before
        // `shipped`; `approved` and `draft` before `shipped` so
        // "approved — first wave shipped, follow-ups in flight"
        // doesn't read as Shipped.
        let lower = value.to_lowercase();
        if lower.contains("superseded") {
            Self::Superseded
        } else if lower.contains("reopened") {
            Self::Reopened
        } else if lower.contains("approved") || lower.contains("accepted") {
            Self::Approved
        } else if lower.contains("draft") {
            Self::Draft
        } else if lower.contains("living") || lower.contains("load-bearing") {
            Self::Living
        } else if lower.contains("shipped") {
            Self::Shipped
        } else {
            Self::InReview
        }
    }

    /// Strict inverse of [`Self::as_str`] — for DB round-trips, where
    /// the value was written by `as_str` and anything else is storage
    /// corruption (unlike [`Self::from_status_line`], which parses
    /// human prose). Adding an enum variant without extending `as_str`
    /// fails here loudly; the schema CHECK + the
    /// `schema_matches_doc_status_enum` pg test pin the DB side.
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "in-review" => Some(Self::InReview),
            "approved" => Some(Self::Approved),
            "shipped" => Some(Self::Shipped),
            "reopened" => Some(Self::Reopened),
            "superseded" => Some(Self::Superseded),
            "living" => Some(Self::Living),
            _ => None,
        }
    }

    /// Does this status assert there is no open discussion? Shipped +
    /// Superseded (the work completed) and Living (a settled
    /// reference). A doc in any of these states must have zero
    /// unresolved open questions — reindex rejects otherwise; the
    /// author flips to `reopened` before adding a `### Qn:`.
    pub fn forbids_open_questions(&self) -> bool {
        matches!(self, Self::Shipped | Self::Superseded | Self::Living)
    }
}

/// A parsed open question from a design doc's `## Open questions` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignQuestion {
    pub id: String,                 // '{doc_path}#Q1'
    pub doc_path: String,           // 'docs/design/correctness-protocol.md'
    pub anchor: String,             // 'Q1' or '{slug}-open-0' fallback
    pub ordinal: i32,               // 0-based position within the section
    pub title: String,              // heading text after 'Q1:'
    pub body_md: String,            // full markdown body of the question
    pub proposal: Option<String>,   // parsed from '**Proposal**: ...'
    pub context_md: Option<String>, // surrounding paragraphs for display
}

/// Metadata snapshot for a design doc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignDoc {
    pub path: String,
    pub title: String,
    pub status: DocStatus,
    pub pending_count: i32,
    pub word_count: i32,
    pub last_modified: DateTime<Utc>,
    pub last_author: String,
    pub last_indexed_at: DateTime<Utc>,
    pub last_commit_sha: String,
    pub content_html: String,
}

/// A pending decision — the human has clicked through a question but
/// not yet flushed it to git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDecision {
    pub id: String,
    pub doc_path: String,
    pub anchor: String,
    pub kind: DecisionKind,
    pub resolution: String,
    pub rationale: Option<String>,
    pub decided_by: String,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Accept,
    Override,
}

impl DecisionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Override => "override",
        }
    }
}

/// Request body for `POST /api/design/pending-decisions`.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingDecisionInput {
    pub doc_path: String,
    pub anchor: String,
    pub kind: DecisionKind,
    pub resolution: String,
    pub rationale: Option<String>,
}

/// A flush job. Immutable payload snapshot + worker status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushJob {
    pub id: String,
    pub doc_path: String,
    pub status: JobStatus,
    pub requested_by: String,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub payload: FlushJobPayload,
    pub commit_sha: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

/// Immutable snapshot of pending decisions captured at flush-job
/// creation time. The worker consumes this payload and produces a
/// commit that embeds each decision into the target markdown file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushJobPayload {
    pub doc_path: String,
    pub base_commit_sha: String,
    pub decisions: Vec<FlushDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlushDecision {
    pub anchor: String,
    pub kind: DecisionKind,
    pub resolution: String,
    pub rationale: Option<String>,
}

/// Body for `PUT /api/design/flush-jobs/:id` when the worker updates
/// status. Not every field has to be set on every call.
#[derive(Debug, Clone, Deserialize)]
pub struct JobStatusUpdate {
    pub status: JobStatus,
    #[serde(default)]
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[cfg(test)]
mod status_parse_tests {
    use super::DocStatus;

    #[test]
    fn leading_token_wins_over_inline_keyword() {
        // Real-world regression that produced the indexer warning:
        // sim-engines.md said "living — HumanWorker engines shipped …"
        // and the substring matcher false-positived on "shipped".
        let status = DocStatus::from_status_line(
            "living — HumanWorker / Counterparty / Periodic engines shipped; \
             BatchEngine archetype in design with 6 open questions tracked.",
        );
        // The leading token still wins — and since 2026-07-08 `living`
        // is its own status, not an InReview alias.
        assert_eq!(status, DocStatus::Living);
    }

    #[test]
    fn approved_with_phase_shipped_in_prose_stays_approved() {
        let status = DocStatus::from_status_line(
            "approved — step 1 (deferred-revenue schema) shipped; steps 2–6 tracked.",
        );
        assert_eq!(status, DocStatus::Approved);
    }

    #[test]
    fn shipped_leading_word_parses_shipped() {
        assert_eq!(
            DocStatus::from_status_line("shipped — migration complete"),
            DocStatus::Shipped
        );
    }

    #[test]
    fn reopened_leads_over_shipped() {
        assert_eq!(
            DocStatus::from_status_line("reopened — original scope shipped 2026-04-18"),
            DocStatus::Reopened
        );
    }

    #[test]
    fn accepted_is_an_alias_for_approved() {
        assert_eq!(
            DocStatus::from_status_line("accepted — replanned 2026-04-22"),
            DocStatus::Approved
        );
    }

    #[test]
    fn done_is_an_alias_for_shipped() {
        assert_eq!(
            DocStatus::from_status_line("done — all phases landed"),
            DocStatus::Shipped
        );
    }

    #[test]
    fn unknown_leading_word_falls_through_to_substring() {
        // Free-prose statuses without a recognized leading token
        // still resolve via the substring scan.
        assert_eq!(
            DocStatus::from_status_line("this design has been approved by the architecture board"),
            DocStatus::Approved
        );
    }

    #[test]
    fn living_vocabulary_parses_to_living_not_in_review() {
        // The pre-2026-07-08 collapse: every settled reference doc
        // ("living guidance", "load-bearing thesis", "stable") read as
        // in-review, so the tracker could not distinguish docs under
        // active discussion from references nobody is acting on.
        for line in [
            "living guidance — describes how the codebase actually tests",
            "living — HumanWorker engine shipped, BatchEngine in design",
            "load-bearing thesis. Treat this as the design north star",
            "stable — describes the JobKind v2 extensibility model.",
        ] {
            assert_eq!(
                DocStatus::from_status_line(line),
                DocStatus::Living,
                "{line:?}"
            );
        }
        // Active-discussion vocabulary stays InReview.
        assert_eq!(
            DocStatus::from_status_line("open — design for the WIP item"),
            DocStatus::InReview
        );
        assert_eq!(
            DocStatus::from_status_line("in-review"),
            DocStatus::InReview
        );
    }

    #[test]
    fn living_forbids_open_questions() {
        assert!(DocStatus::Living.forbids_open_questions());
        assert!(DocStatus::Shipped.forbids_open_questions());
        assert!(!DocStatus::Reopened.forbids_open_questions());
        assert!(!DocStatus::InReview.forbids_open_questions());
    }
}
