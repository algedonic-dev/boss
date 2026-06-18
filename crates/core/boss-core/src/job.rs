//! Shared types for the Job coordination primitive.
//!
//! A Job is a bounded unit of coordinated work — device repair, procurement,
//! sales cycle, marketing campaign, employee onboarding. Jobs decompose into
//! Steps, each owned by a person or team, with optional sign-off gates
//! and cross-job dependency tracking.
//!
//! These types live in boss-core because they are shared vocabulary: every
//! domain crate can reference a JobId or Subject without depending on the
//! boss-jobs service crate.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::define_id;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

define_id!(JobId);
define_id!(StepId);

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// What the Job is about — a (kind, id) tuple referencing any
/// identity-bearing entity in the system.
///
/// Wire shape: `{"subject_kind": "<kind>", "id": "..."}`.
///
/// Per the founding-invariant that taxonomies are data, Subject is a
/// (kind, id) pair validated against the boss-subject-kinds registry
/// — not a closed Rust enum. Tenants add a new SubjectKind by
/// registering a row, never by forking core.
///
/// Core owns only the **mechanism** — the (kind, id) shape, `new`,
/// and the kind/id accessors. The **vocabulary** (the five root noun
/// axes and every specialization — `asset`, `account`, `vendor`,
/// `purchase_order`, …) lives in the SubjectKind registry as data,
/// the single source of truth. Tier-1 core names no business noun:
/// callers build kinds with `Subject::new("<kind>", id)`, the kind
/// string being the data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    #[serde(rename = "subject_kind")]
    pub kind: String,
    pub id: String,
}

impl crate::primitives::Subject for Subject {
    fn kind(&self) -> &str {
        &self.kind
    }
    fn id(&self) -> &str {
        &self.id
    }
}

impl Subject {
    /// Build a Subject of any kind. The kind is data — a string
    /// validated against the SubjectKind registry at write time, not a
    /// closed set core enumerates. There are deliberately no
    /// per-kind constructors: core names no business noun.
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
        }
    }
}

/// Lifecycle status of a Job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Draft,
    Open,
    Blocked,
    PendingSignOff,
    Closed,
    Cancelled,
}

/// Job priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    Emergency,
    Urgent,
    Standard,
    Scheduled,
}

/// Status of a single Step line item.
///
/// Five statuses describing one predicate-driven lifecycle. Each
/// Step carries a `ready_when` predicate (declared on its JobKind
/// `StepSpec`); the materializer evaluates it at Job open and the
/// re-evaluator re-checks it on every upstream change. Status is the
/// program counter — which nodes of the JobKind's implicit DAG have
/// fired, which are eligible, which the predicates ruled out.
///
/// ```text
///   pending ─(ready_when true)─▶ ready ─(assigned)─▶ active ─(complete)─▶ completed
///      │
///      └─(ready_when provably false-forever)─▶ skipped
/// ```
///
/// No `Blocked` / `Aborted`: a step paused on a dependency is
/// simply `Pending` until its predicate flips, and an abandoned
/// branch is `Skipped`. Reactions to external state live in
/// dispatcher rules, not in the step lifecycle. See
/// docs/architecture-decisions.md §Jobs, JobKinds, Steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StepStatus {
    /// `ready_when` evaluates false — planned but not yet eligible.
    /// Rendered greyed out.
    #[default]
    Pending,
    /// `ready_when` evaluates true — eligible, awaiting an executor
    /// to pick up the work.
    Ready,
    /// An executor picked up the step; work is in flight.
    Active,
    /// Done successfully. Metadata is committed; downstream
    /// predicates may now re-evaluate. Reaching `Completed` on a
    /// *terminal* step closes the Job with that step's outcome.
    Completed,
    /// `ready_when` is provably unsatisfiable (every upstream step it
    /// references reached a terminal state and the predicate is still
    /// false), or a terminal closed the Job before this step ran.
    /// Rendered struck-through — "not applicable."
    Skipped,
}

// ---------------------------------------------------------------------------
// Aggregates
// ---------------------------------------------------------------------------

/// The coordination envelope — one bounded unit of work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    #[serde(default)]
    pub id: JobId,
    pub kind: String,
    pub subject: Subject,
    pub title: String,
    pub owner_id: String,
    pub status: JobStatus,
    pub priority: Priority,
    pub opened_on: NaiveDate,
    pub due_on: Option<NaiveDate>,
    pub closed_on: Option<NaiveDate>,
    pub metadata: serde_json::Value,
    pub tags: Vec<String>,
}

impl Job {
    pub fn new(
        kind: impl Into<String>,
        subject: Subject,
        title: impl Into<String>,
        owner_id: impl Into<String>,
        priority: Priority,
        opened_on: NaiveDate,
    ) -> Self {
        Self {
            id: JobId::new(),
            kind: kind.into(),
            subject,
            title: title.into(),
            owner_id: owner_id.into(),
            status: JobStatus::Draft,
            priority,
            opened_on,
            due_on: None,
            closed_on: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            tags: Vec::new(),
        }
    }

    pub fn with_due_on(mut self, due_on: NaiveDate) -> Self {
        self.due_on = Some(due_on);
        self
    }

    /// Override the auto-generated `JobId` with one supplied by
    /// the caller. Used by sim/replay paths that derive IDs from
    /// a seeded RNG so two runs with identical inputs produce
    /// byte-identical output (correctness-protocol property 5).
    /// Production callers that don't need replay determinism
    /// stay on the `Job::new` default.
    pub fn with_id(mut self, id: JobId) -> Self {
        self.id = id;
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// A step-authored completion-contract field (inline authoring —
/// architecture-decisions.md §Step types are property bundles):
/// the same schema language registry bundles carry,
/// declared per step in the JobKind. Validation is the union of the
/// bundle's fields and these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepField {
    pub name: String,
    pub field_type: String,
    #[serde(default)]
    pub required: bool,
}

/// One sign-off stamp (architecture-decisions.md §Step types are
/// property bundles): an authenticated authority's
/// attestation of a step *in its current shape*. Policy-checked at
/// stamp time and recorded as its own audit event; `shape_hash`
/// binds the stamp to the content it attested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignOffStamp {
    /// Who stamped — an employee id (stamping is a human act; the
    /// policy-gated external-override path records its own stamp).
    pub authority_id: String,
    /// The required-role this stamp satisfies.
    pub role: String,
    pub stamped_at: chrono::DateTime<chrono::Utc>,
    /// `step_shape_hash` of the step when stamped.
    pub shape_hash: String,
}

/// Hash of a step's completion-relevant content — what a sign-off
/// stamp attests. Title + metadata, canonically serialized (sorted
/// keys) so hashing is insertion-order independent. Fields that
/// don't change what is being agreed to (status, assignee, sort
/// order, plugin pin) are deliberately excluded.
pub fn step_shape_hash(title: &str, metadata: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(v: &serde_json::Value, out: &mut Vec<u8>) {
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                out.push(b'{');
                for k in keys {
                    out.extend_from_slice(k.as_bytes());
                    out.push(b':');
                    canonical(&m[k], out);
                    out.push(b',');
                }
                out.push(b'}');
            }
            serde_json::Value::Array(a) => {
                out.push(b'[');
                for x in a {
                    canonical(x, out);
                    out.push(b',');
                }
                out.push(b']');
            }
            other => out.extend_from_slice(other.to_string().as_bytes()),
        }
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(title.as_bytes());
    buf.push(0);
    canonical(metadata, &mut buf);
    let mut h = Sha256::new();
    h.update(&buf);
    hex::encode(h.finalize())
}

/// A line item on a Job — one typed unit of work that must be completed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(default)]
    pub id: StepId,
    #[serde(default)]
    pub job_id: JobId,
    #[serde(default = "default_step_kind")]
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub assignee_id: Option<String>,
    #[serde(default)]
    pub status: StepStatus,
    #[serde(default)]
    pub sort_order: i32,
    /// Upstream steps this step's `ready_when` predicate references,
    /// derived at materialization. Not the gate — the predicate is —
    /// but a denormalized edge list the SPA renders the DAG from and
    /// the re-evaluator keys its dependency index on.
    #[serde(default)]
    pub blocked_by: Vec<StepId>,
    /// Role codes that must each stamp this step in its current shape
    /// before it may complete (the sign-off contract). Materialized from the
    /// step type's bundle (with `@authority_role` resolved to this
    /// step's authority_role). Empty = no sign-offs required.
    #[serde(default)]
    pub sign_offs_required: Vec<String>,
    /// Step-authored completion-contract fields (inline authoring): validated
    /// at completion in union with the step type's bundle fields.
    #[serde(default)]
    pub fields: Vec<StepField>,
    /// Stamps collected so far. A stamp attests the step *in the
    /// shape it had when stamped* (`shape_hash`); completion counts
    /// only stamps whose hash matches the current shape. Stale stamps
    /// stay recorded — they are provenance, not validity.
    #[serde(default)]
    pub sign_offs: Vec<SignOffStamp>,
    #[serde(default)]
    pub completed_on: Option<NaiveDate>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub notes: Option<String>,
    /// Plugin version snapshotted at step creation time. Zero means
    /// "no plugin" (the step renders through an in-tree surface) or
    /// "snapshot not taken yet". The writer sets it once on insert by
    /// looking up the active plugin for the step's kind; republishing
    /// the plugin later doesn't retroactively change which bundle a
    /// long-running job's step is pinned against.
    #[serde(default)]
    pub step_plugin_version: i32,
    /// Optional pointer to a child Job when this Step's work
    /// decomposes further. `Some(id)` means "traversal into this
    /// Step descends into that Job's step graph" — the structural
    /// realization of Step-as-Job recursion. Default
    /// `None` for ordinary steps.
    #[serde(default)]
    pub embedded_job: Option<JobId>,
}

fn default_step_kind() -> String {
    "generic".to_string()
}

fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl Step {
    pub fn new(
        job_id: JobId,
        kind: impl Into<String>,
        title: impl Into<String>,
        sort_order: i32,
    ) -> Self {
        Self {
            id: StepId::new(),
            job_id,
            kind: kind.into(),
            title: title.into(),
            assignee_id: None,
            // New Steps default to Pending (ready_when not yet
            // satisfied). The re-evaluator promotes Pending → Ready
            // when the step's predicate first evaluates true. See the
            // `StepStatus` doc comment.
            status: StepStatus::Pending,
            sort_order,
            blocked_by: Vec::new(),
            sign_offs_required: Vec::new(),
            sign_offs: Vec::new(),
            fields: Vec::new(),
            completed_on: None,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            notes: None,
            step_plugin_version: 0,
            embedded_job: None,
        }
    }

    pub fn with_assignee(mut self, assignee_id: impl Into<String>) -> Self {
        self.assignee_id = Some(assignee_id.into());
        self
    }

    pub fn with_sign_offs_required(mut self, roles: Vec<String>) -> Self {
        self.sign_offs_required = roles;
        self
    }

    /// True when every required role has a stamp attesting the step's
    /// *current* shape. Stale stamps (collected before a
    /// later edit) don't count.
    pub fn sign_offs_satisfied(&self) -> bool {
        if self.sign_offs_required.is_empty() {
            return true;
        }
        let current = step_shape_hash(&self.title, &self.metadata);
        self.sign_offs_required.iter().all(|role| {
            self.sign_offs
                .iter()
                .any(|st| &st.role == role && st.shape_hash == current)
        })
    }

    pub fn with_blocked_by(mut self, blocked_by: Vec<StepId>) -> Self {
        self.blocked_by = blocked_by;
        self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_unique() {
        let a = JobId::new();
        let b = JobId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn step_id_unique() {
        let a = StepId::new();
        let b = StepId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn job_serde_round_trip() {
        let job = Job::new(
            "test-kind",
            Subject::new("asset", "sys-001"),
            "Test job",
            "emp-42",
            Priority::Standard,
            NaiveDate::from_ymd_opt(2026, 4, 16).unwrap(),
        );
        let json = serde_json::to_string(&job).unwrap();
        let back: Job = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn step_serde_round_trip() {
        let job_id = JobId::new();
        let step = Step::new(job_id, "generic", "QA passed", 3)
            .with_assignee("emp-10")
            .with_sign_offs_required(vec!["qa-lead".into()]);
        let json = serde_json::to_string(&step).unwrap();
        let back: Step = serde_json::from_str(&json).unwrap();
        assert_eq!(step, back);
    }

    #[test]
    fn subject_serializes_with_tag() {
        // Subject is uniformly (kind, id). Wire shape is
        // `{"subject_kind": "<kind>", "id": "..."}` regardless of
        // whether the kind is a platform kind or tenant-defined.
        let s = Subject::new("asset", "SN-001");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""subject_kind":"asset""#));
        assert!(json.contains(r#""id":"SN-001""#));

        let s = Subject::new("asset", "A-1");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains(r#""subject_kind":"asset""#));
        assert!(json.contains(r#""id":"A-1""#));
    }

    #[test]
    fn job_status_kebab_case() {
        let s = JobStatus::PendingSignOff;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#""pending-sign-off""#);

        let back: JobStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, JobStatus::PendingSignOff);
    }

    #[test]
    fn priority_kebab_case() {
        let p = Priority::Emergency;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#""emergency""#);
    }

    #[test]
    fn step_status_kebab_case() {
        let s = StepStatus::Active;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#""active""#);
    }

    #[test]
    fn job_new_defaults() {
        let job = Job::new(
            "test-kind",
            Subject::new("account", "acc-001"),
            "Test job",
            "emp-1",
            Priority::Urgent,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        assert_eq!(job.status, JobStatus::Draft);
        assert_eq!(job.metadata, serde_json::json!({}));
        assert!(job.tags.is_empty());
        assert!(job.due_on.is_none());
        assert!(job.closed_on.is_none());
    }

    #[test]
    fn step_new_defaults() {
        let step = Step::new(JobId::new(), "generic", "Do the thing", 0);
        assert_eq!(step.status, StepStatus::Pending);
        assert!(step.sign_offs_required.is_empty());
        assert!(step.sign_offs.is_empty());
        assert!(step.assignee_id.is_none());
        assert!(step.blocked_by.is_empty());
        assert_eq!(step.metadata, serde_json::json!({}));
    }

    #[test]
    fn shape_hash_is_key_order_independent() {
        let a = serde_json::json!({"qty": 5, "sku": "X"});
        let b = serde_json::json!({"sku": "X", "qty": 5});
        assert_eq!(step_shape_hash("t", &a), step_shape_hash("t", &b));
    }

    #[test]
    fn shape_hash_changes_with_content() {
        let a = serde_json::json!({"qty": 5});
        let b = serde_json::json!({"qty": 6});
        assert_ne!(step_shape_hash("t", &a), step_shape_hash("t", &b));
        assert_ne!(step_shape_hash("t", &a), step_shape_hash("u", &a));
    }

    #[test]
    fn cross_job_blocked_by() {
        let job_a = JobId::new();
        let job_b = JobId::new();
        let step_a = Step::new(job_a, "generic", "Step A", 0);
        let step_b = Step::new(job_b, "generic", "Step B", 0).with_blocked_by(vec![step_a.id]);
        assert_eq!(step_b.blocked_by.len(), 1);
        assert_eq!(step_b.blocked_by[0], step_a.id);
    }
}
