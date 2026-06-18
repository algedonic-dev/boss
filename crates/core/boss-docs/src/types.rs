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
    /// - `living` / `in-review` / `open` → InReview
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
            "living" | "in-review" | "open" => Some(Self::InReview),
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
        } else if lower.contains("shipped") {
            Self::Shipped
        } else {
            Self::InReview
        }
    }

    /// Does this status claim the work is completed? Shipped + Superseded
    /// are the two. A doc in either state must have zero unresolved
    /// open questions — reindex rejects otherwise.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Shipped | Self::Superseded)
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
        assert_eq!(status, DocStatus::InReview);
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
}
