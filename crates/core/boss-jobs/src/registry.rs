//! Job Kind Registry — see docs/architecture-decisions.md §Jobs,
//! JobKinds, Steps.
//!
//! Every Job is an instance of a JobKind. This module defines the
//! registry shape (`JobKindSpec`), the port every adapter implements
//! (`JobKindRegistry`), and the in-memory + postgres adapters.
//!
//! The registry is append-only: every edit to a kind creates a new
//! `(kind, version+1)` row. Only one row per `kind` has
//! `status = Active` at a time, enforced by a partial unique index
//! on the Postgres side and by an invariant in the in-memory adapter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::step_registry::{Completion, StepRegistry};
use async_trait::async_trait;
use boss_core::job::{JobId, Step, StepId, StepStatus, Subject};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobKindStatus {
    Draft,
    Active,
    Retired,
}

impl JobKindStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }
}

impl std::str::FromStr for JobKindStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "draft" => Ok(Self::Draft),
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            other => Err(format!("unknown job kind status: {other}")),
        }
    }
}

/// One step in a JobKind: a typed unit of work plus the predicate
/// that decides when it becomes eligible to run.
///
/// `title` is a stable kebab-case slug, unique within the JobKind.
/// Predicates reference it as `steps.<title>.done` /
/// `steps.<title>.metadata.<field>`; the implicit step DAG is
/// recovered from which titles each `ready_when` mentions. The
/// human-facing label comes from `title_template` (or a humanized
/// `title` when that's blank).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSpec {
    /// Stable slug + predicate identifier, unique within the
    /// JobKind. Kebab-case (`mash-in`, `cfo-approval`).
    pub title: String,
    /// StepType slug from the StepType registry.
    pub kind: String,
    /// Predicate over the workflow's state — `true` when this step
    /// is eligible to advance from `pending` to `ready`. Evaluated
    /// by the shared `boss-expr` DSL against `(subject, job
    /// metadata, prior step states)`. `"true"` marks a trigger
    /// step that fires at Job open. Vocabulary:
    /// docs/architecture-decisions.md §Jobs, JobKinds, Steps.
    pub ready_when: String,
    /// Marks this step as an outcome. Reaching `Completed` on a
    /// terminal step closes the Job and stamps the outcome label.
    /// Multiple terminals per JobKind is normal (success /
    /// rejection / abandonment paths all terminate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<Terminal>,
    /// Human display label; `{subject.<field>}` + `{day…}` tokens
    /// expand at materialization. Blank → humanized `title`.
    #[serde(default)]
    pub title_template: String,
    /// Role codes that must each stamp the materialized step in its
    /// current shape before completion (the sign-off contract). The marker
    /// `"@authority_role"` resolves to this step's `authority_role`
    /// at materialization.
    #[serde(default)]
    pub sign_offs_required: Vec<String>,
    /// Step-authored completion-contract fields (inline
    /// authoring) — validated in union with the kind bundle's fields,
    /// so vocabulary that isn't shared needs no registry row.
    #[serde(default)]
    pub fields: Vec<boss_core::job::StepField>,
    #[serde(default)]
    pub authority_role: Option<String>,
    #[serde(default)]
    pub metadata_defaults: serde_json::Value,
}

/// An outcome marker on a terminal step. Reaching `Completed` on a
/// step that carries one closes the Job with this outcome label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    /// User-facing outcome: "succeeded" | "rejected" | "abandoned"
    /// | "cancelled" + domain-specific ("paid", "shipped",
    /// "skipped", "resold").
    pub outcome: String,
}

/// A cross-Job trigger declaration. When a Job of the parent
/// JobKind transitions to `closed`, the runtime spawns one new Job
/// per entry in `on_complete_create`.
///
/// A rule like "completing a wholesale-order Job creates an invoice
/// Job" lives as a row on the parent JobKind, the same way the step
/// graph does — not as a branch in core code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobTrigger {
    /// JobKind slug to spawn. Must resolve to an active JobKind
    /// row when the trigger fires; an unresolvable kind logs a
    /// warning and the trigger is skipped (not fatal).
    pub kind: String,
    /// Where the new Job's subject comes from:
    ///
    /// - `"same"` (default) — reuse the closing Job's subject
    ///   verbatim. Common for chained workflows that share an
    ///   anchor (a sale Job and its invoice both reference the
    ///   same customer).
    /// - `"metadata:<key>"` — read the closing Job's
    ///   `metadata.<key>` as a string id; the new Job's
    ///   subject_kind comes from the spawned JobKind's first
    ///   `subject_kinds` entry.
    #[serde(default = "default_subject_source")]
    pub subject_source: String,
    /// Static metadata to seed onto the new Job. Values may use
    /// `{closing.metadata.<key>}` placeholders; the runtime expands
    /// them at trigger time. Empty object = no seed.
    #[serde(default)]
    pub metadata_seed: serde_json::Value,
}

fn default_subject_source() -> String {
    "same".to_string()
}

/// A full registry row. All fields serialize directly to the
/// `job_kinds` JSONB columns with the same names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobKindSpec {
    pub kind: String,
    pub version: i32,
    pub status: JobKindStatus,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    pub category: String,
    pub subject_kinds: Vec<String>,
    /// The flat set of steps. The DAG is implicit in the steps'
    /// `ready_when` predicates — no separate tier / edge structure.
    pub steps: Vec<StepSpec>,
    #[serde(default)]
    pub metadata_schema: serde_json::Value,
    #[serde(default)]
    pub entitlements: serde_json::Value,
    /// JobKind-level display/routing hints — distinct from the
    /// per-Job metadata and from `metadata_schema` (which describes
    /// per-Job fields). The first key is `surfaces`: which
    /// operational pages a JobKind appears on. Same
    /// `serde_json::Value` round-trip shape as `metadata_schema` /
    /// `entitlements`; serializes to the `metadata` JSONB column.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// JobKinds to spawn when a Job of this kind closes. The
    /// runtime that fires triggers reads this list off the row;
    /// data shape is shipped here, the runtime hook lands in a
    /// follow-up. Empty list = no triggers (the common case).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_complete_create: Vec<JobTrigger>,
    pub owning_team: String,
    #[serde(default)]
    pub authoring_job_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl JobKindSpec {
    /// Build a system-owned active v1 row for seeding.
    pub fn platform_seed(
        kind: impl Into<String>,
        label: impl Into<String>,
        category: impl Into<String>,
        subject_kinds: Vec<String>,
        steps: Vec<StepSpec>,
    ) -> Self {
        Self {
            kind: kind.into(),
            version: 1,
            status: JobKindStatus::Active,
            label: label.into(),
            description: None,
            category: category.into(),
            subject_kinds,
            steps,
            metadata_schema: serde_json::json!({}),
            entitlements: serde_json::json!({}),
            metadata: serde_json::json!({}),
            on_complete_create: Vec::new(),
            owning_team: "platform".to_string(),
            authoring_job_id: None,
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Platform kinds — code-resident JobKinds that the bootstrap
// reconciler upserts into the live registry on every
// boss-jobs-api start. The single platform kind today is
// `job-kind-design`, the meta-kind that authors every other
// kind in the registry. See docs/architecture-decisions.md
// §Jobs, JobKinds, Steps (JobKinds bootstrap through Jobs).
// ---------------------------------------------------------------------------

/// Build the canonical `job-kind-design` JobKindSpec.
///
/// Step graph (tier-major, default edges between adjacent tiers):
/// 0. `task`              — Author spec
/// 1. `task`              — Validate (lint via `validate_all`)
/// 2. `sign-off`          — Approve (authority_role = `job-kind-approver`)
/// 3. `job-kind-publish`  — Publish (writes to registry, emits
///    `jobs.kind.published`)
///
/// Subject discriminator is `custom`, with `custom_kind =
/// "job-kind"` per Q3 of the design doc — reuses the existing
/// CustomSubject support without forcing a new Subject variant
/// in `boss-core`.
fn job_kind_design_spec() -> JobKindSpec {
    let steps = vec![
        StepSpec {
            title: "author".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Author JobKindSpec".into(),
            ..Default::default()
        },
        StepSpec {
            title: "validate".into(),
            kind: "task".into(),
            ready_when: "steps.author.done".into(),
            title_template: "Validate spec".into(),
            ..Default::default()
        },
        StepSpec {
            title: "approve".into(),
            kind: "sign-off".into(),
            ready_when: "steps.validate.done".into(),
            title_template: "Approve spec".into(),
            // Approval authority is `job-kind-approver` — an operational-
            // leadership capability granted (via tenant policy) to the
            // C-suite/COO/dept-heads who own the `job-kinds` authoring
            // surface, plus platform-admin (core policy default). NOT
            // platform-admin alone: authoring a work-type is the
            // operational leaders' job, not solely the deploy operator's.
            sign_offs_required: vec!["job-kind-approver".into()],
            authority_role: Some("job-kind-approver".into()),
            metadata_defaults: serde_json::json!({ "authority_role": "job-kind-approver" }),
            ..Default::default()
        },
        StepSpec {
            title: "publish".into(),
            kind: "job-kind-publish".into(),
            ready_when: "steps.approve.done".into(),
            title_template: "Publish to registry".into(),
            terminal: Some(Terminal {
                outcome: "published".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = JobKindSpec::platform_seed(
        "job-kind-design",
        "Design a JobKind",
        "platform",
        vec!["custom".into()],
        steps,
    );
    // Q7: the responsible human for platform meta-work. The approve
    // step's authority (`job-kind-approver`) is a policy CAPABILITY,
    // not an employees.role value, so the step-authority fallback
    // can't resolve it — name the operator-baseline role explicitly.
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "Meta-kind: every JobKind in the registry is authored by a Job of this kind. \
         The terminal `job-kind-publish` step writes the spec into the registry and \
         emits `jobs.kind.published` into audit_log. See \
         docs/architecture-decisions.md (Jobs, JobKinds, Steps)."
            .to_string(),
    );
    spec
}

/// Every JobKind that ships baked into the platform binary. Read by
/// `boss-jobs-api`'s startup reconciler — `kind_registry
/// .bootstrap_reconcile(&platform_kinds())` runs on every boot,
/// inserting missing rows / refreshing drifted bootstrap-owned
/// rows / preserving operator-edited rows.
///
/// Today: `job-kind-design` + `design-doc-review`. Future platform
/// kinds (a `step-plugin-design` meta-kind, perhaps a
/// `policy-rule-design` one) land here as additional entries —
/// never as TOML-loader exceptions, never as direct
/// `INSERT INTO job_kinds` SQL.
pub fn platform_kinds() -> Vec<JobKindSpec> {
    vec![job_kind_design_spec(), design_doc_review_spec()]
}

/// Build the canonical `design-doc-review` JobKindSpec.
///
/// The "system models its own development" workflow: a design doc
/// under `docs/design/` becomes the Subject of one of these Jobs; the
/// review-design step (custom Step UX plugin) gates completion on
/// every open question being addressed.
///
/// Step graph:
/// -1. `trigger`        — author opened design doc for review
///  0. `review-design`  — plugin-rendered: lists open questions,
///                        captures a resolution per question,
///                        blocks completion until all questions
///                        have a resolution recorded. Resolutions
///                        flow into the existing
///                        `/api/design/pending-decisions` rows;
///                        `/api/design/flush-jobs` extracts them
///                        to ADRs.
///  999. `outcome`      — review complete; decisions captured
fn design_doc_review_spec() -> JobKindSpec {
    let steps = vec![
        StepSpec {
            title: "open".into(),
            kind: "trigger".into(),
            ready_when: "true".into(),
            title_template: "Design doc opened for review".into(),
            metadata_defaults: serde_json::json!({
                "trigger_kind": "operator",
                "trigger_name": "design-doc-author-opens-review",
            }),
            ..Default::default()
        },
        StepSpec {
            title: "review".into(),
            kind: "review-design".into(),
            ready_when: "steps.open.done".into(),
            title_template: "Review design doc — address open questions".into(),
            authority_role: Some("platform-admin".into()),
            // doc_path is stamped AT MATERIALIZATION from the Job's
            // subject (the doc path IS the subject id) — atomically,
            // in the same write that creates the step. It must never
            // rely on a follow-up PUT: a second write loses
            // read-overlay-write races against dispatcher assignment
            // and workforce completion, and terminal-metadata
            // immutability then seals the empty value (the
            // 2026-07-14 "doc_path is empty" incident).
            metadata_defaults: serde_json::json!({
                "doc_path": "{subject.id}",
                "resolutions": [],
            }),
            ..Default::default()
        },
        StepSpec {
            title: "reviewed".into(),
            kind: "outcome".into(),
            ready_when: "steps.review.done".into(),
            title_template: "Design reviewed — decisions captured".into(),
            metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
            terminal: Some(Terminal {
                outcome: "completed".into(),
            }),
            ..Default::default()
        },
    ];

    let mut spec = JobKindSpec::platform_seed(
        "design-doc-review",
        "Review a design doc",
        "platform",
        vec!["custom".into()],
        steps,
    );
    // Q7: same platform-admin ownership as job-kind-design — the
    // review step's authority is platform-admin already, but the
    // explicit owner_role keeps both meta-kinds resolvable even if
    // step shapes change.
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec.description = Some(
        "Meta-kind: every design doc under docs/design/ gets reviewed via a Job of this kind. \
         The `review-design` step uses a custom Step UX plugin that reads the doc's open \
         questions (parsed by boss-docs-api from `### Qn:` headings) and gates completion \
         until each one has a recorded resolution. Resolutions land in \
         `/api/design/pending-decisions`; subsequent `/api/design/flush-jobs` writes them \
         into the source doc's Decision-history section (each release, settled material \
         folds into `docs/architecture-decisions.md` and the source doc is deleted). \
         Replaces the in-app decision-tracker surface retired on 2026-05-03."
            .to_string(),
    );
    spec
}

// ---------------------------------------------------------------------------
// Materialization — turn a spec + subject into concrete Steps
// ---------------------------------------------------------------------------

/// Flatten a Subject into `{subject.<field>}` lookup pairs for title
/// template expansion. A Subject is uniformly `(kind, id)`, so this
/// exposes `{subject.id}` and `{subject.kind}` for every kind.
fn subject_fields(subject: &Subject) -> Vec<(&'static str, &str)> {
    // Subject is uniformly (kind, id). Templates use
    // `{subject.id}` and `{subject.kind}`.
    vec![("id", subject.id.as_str()), ("kind", subject.kind.as_str())]
}

fn expand_title(template: &str, subject: &Subject) -> String {
    let mut out = template.to_string();
    for (field, value) in subject_fields(subject) {
        let token = format!("{{subject.{field}}}");
        out = out.replace(&token, value);
    }
    out
}

/// Recursive `{subject.<field>}` + `{day...}` substitution over
/// a JSON metadata-defaults blob. Walks every string leaf in the
/// value (object keys + array elements + nested objects). Keys
/// are not substituted — the placeholder lives in values.
///
/// Supported tokens:
///   - `{subject.<field>}` — per-Job subject context (every call)
///   - `{day}` — the Job's open day, when `date_anchor` is `Some`
///   - `{day_minus_<N>}` — open day minus N calendar days
///   - `{day_plus_<N>}` — open day plus N calendar days
///
/// Date substitution is opt-in via `date_anchor`. When `None`,
/// `{day...}` tokens stay as literal placeholders — that keeps
/// the live-API path (which has no sim day) honest about
/// "this template requires a date anchor" instead of silently
/// stamping `Utc::now()`. The brewery's biweekly-payroll JobKind
/// uses this for `period_start = "{day_minus_13}"` so each
/// run's period derives from the sim day instead of a hardcoded
/// 2026-01-* literal (which left the labor-absorption credit
/// to 6100 Payroll un-offset by the payroll DR within the same
/// FY and broke period close).
fn expand_metadata(
    template: &serde_json::Value,
    subject: &Subject,
    job_metadata: &serde_json::Value,
    date_anchor: Option<chrono::NaiveDate>,
) -> serde_json::Value {
    use serde_json::Value;
    let fields = subject_fields(subject);
    // Scalar Job-metadata fields, exposed as `{metadata.<field>}` so a spawned
    // Job can parameterize its steps' metadata_defaults — e.g. the reorder rule
    // passes the triggering `part_sku` into the restock's PO line so each
    // restock buys one ingredient instead of a fixed full-catalog bundle.
    let meta_fields: Vec<(String, String)> = job_metadata
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| match v {
                    Value::String(s) => Some((k.clone(), s.clone())),
                    Value::Number(n) => Some((k.clone(), n.to_string())),
                    Value::Bool(b) => Some((k.clone(), b.to_string())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    fn substitute_day(out: &mut String, day: chrono::NaiveDate) {
        // Cheap-and-clean exact replace for {day}, then offset
        // tokens via a small regex-free scan. The token alphabet
        // is `{day_minus_<digits>}` / `{day_plus_<digits>}` —
        // anything else is left alone.
        *out = out.replace("{day}", &day.format("%Y-%m-%d").to_string());
        for prefix in ["{day_minus_", "{day_plus_"] {
            while let Some(start) = out.find(prefix) {
                let after = start + prefix.len();
                let Some(end_rel) = out[after..].find('}') else {
                    break;
                };
                let end = after + end_rel;
                let digits = &out[after..end];
                let Ok(n) = digits.parse::<i64>() else {
                    // Malformed token — replace with itself to avoid an
                    // infinite loop, then bail.
                    let token = &out[start..=end].to_string();
                    *out = out.replacen(token, &format!("__{token}__"), 1);
                    break;
                };
                let offset = if prefix.starts_with("{day_minus_") {
                    -n
                } else {
                    n
                };
                let stamped = (day + chrono::Duration::days(offset))
                    .format("%Y-%m-%d")
                    .to_string();
                let token = &out[start..=end].to_string();
                *out = out.replacen(token, &stamped, 1);
            }
        }
    }
    fn walk(
        v: &serde_json::Value,
        fields: &[(&'static str, &str)],
        meta_fields: &[(String, String)],
        date_anchor: Option<chrono::NaiveDate>,
    ) -> serde_json::Value {
        match v {
            Value::String(s) => {
                let mut out = s.clone();
                for (field, value) in fields {
                    let token = format!("{{subject.{field}}}");
                    out = out.replace(&token, value);
                }
                for (field, value) in meta_fields {
                    let token = format!("{{metadata.{field}}}");
                    out = out.replace(&token, value);
                }
                if let Some(day) = date_anchor {
                    substitute_day(&mut out, day);
                }
                Value::String(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|x| walk(x, fields, meta_fields, date_anchor))
                    .collect(),
            ),
            Value::Object(map) => {
                let mut out = serde_json::Map::with_capacity(map.len());
                for (k, v) in map {
                    out.insert(k.clone(), walk(v, fields, meta_fields, date_anchor));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
    walk(template, &fields, &meta_fields, date_anchor)
}

/// Turn a `JobKindSpec` + concrete `Subject` + fresh `JobId` into
/// ready-to-insert `Step` rows. Step IDs come from the
/// caller-provided closure so deterministic callers (sim) can use a
/// monotonic counter while runtime paths use `StepId::new()`.
///
/// Eager materialization (JobKind v2): **every** step is created at
/// Job open. Each step's `blocked_by` is the set of upstream steps
/// its `ready_when` predicate references; its initial status is
/// decided by [`reevaluate`] against the open-time state (no step
/// done yet) — trigger / subject-gated predicates flip straight to
/// `Ready`, the rest stay `Pending`. `sort_order` is the step's
/// index in `spec.steps`, which [`reevaluate`] relies on to pair a
/// live `Step` back to its `StepSpec`.
///
/// `{day}` tokens in `metadata_defaults` stay literal — call
/// [`materialize_steps_at`] when the open day is known so the
/// payroll / period-end family of fields gets sim-derived dates.
pub fn materialize_steps<F>(
    spec: &JobKindSpec,
    subject: &Subject,
    job_id: JobId,
    job_metadata: &serde_json::Value,
    step_id_fn: F,
) -> Vec<Step>
where
    F: FnMut() -> StepId,
{
    materialize_steps_at(spec, subject, job_id, job_metadata, step_id_fn, None, None)
}

/// Same as [`materialize_steps`] but expands `{day}`,
/// `{day_minus_<N>}`, `{day_plus_<N>}` tokens in step
/// `metadata_defaults` against `date_anchor`. The sim engine passes
/// `Some(day)` so each Job's metadata reflects the sim's clock; live
/// API paths pass `Some(state.clock.now().await.now.date_naive())`.
pub fn materialize_steps_at<F>(
    spec: &JobKindSpec,
    subject: &Subject,
    job_id: JobId,
    job_metadata: &serde_json::Value,
    mut step_id_fn: F,
    date_anchor: Option<chrono::NaiveDate>,
    step_registry: Option<&StepRegistry>,
) -> Vec<Step>
where
    F: FnMut() -> StepId,
{
    // Allocate one StepId per spec step, in order, and remember each
    // step's slug → id so a predicate's `steps.<slug>` references
    // resolve to concrete StepIds for the denormalized `blocked_by`
    // edge list the SPA renders the DAG from.
    let ids: Vec<StepId> = spec.steps.iter().map(|_| step_id_fn()).collect();
    let slug_to_id: std::collections::HashMap<&str, StepId> = spec
        .steps
        .iter()
        .zip(&ids)
        .map(|(s, id)| (s.title.as_str(), *id))
        .collect();

    let mut steps: Vec<Step> = Vec::with_capacity(spec.steps.len());
    for (idx, (spec_step, id)) in spec.steps.iter().zip(&ids).enumerate() {
        let title = if spec_step.title_template.is_empty() {
            humanize_slug(&spec_step.title)
        } else {
            expand_title(&spec_step.title_template, subject)
        };
        let merged = merge_metadata(&spec_step.metadata_defaults, spec_step);
        // Per-Job context flows through `{subject.<field>}` + `{day…}`
        // substitution so side-effect-bound steps emit per-Job-distinct
        // facts instead of identical deterministic defaults.
        let metadata = expand_metadata(&merged, subject, job_metadata, date_anchor);
        let blocked_by: Vec<StepId> = predicate_step_refs(&spec_step.ready_when)
            .iter()
            .filter_map(|slug| slug_to_id.get(slug.as_str()).copied())
            .collect();
        steps.push(Step {
            id: *id,
            job_id,
            kind: spec_step.kind.clone(),
            title,
            assignee_id: None,
            status: StepStatus::Pending,
            sort_order: idx as i32,
            blocked_by,
            sign_offs_required: spec_step
                .sign_offs_required
                .iter()
                .filter_map(|r| {
                    if r == "@authority_role" {
                        spec_step.authority_role.clone()
                    } else {
                        Some(r.clone())
                    }
                })
                .collect(),
            sign_offs: Vec::new(),
            fields: spec_step.fields.clone(),
            completed_on: None,
            metadata,
            notes: None,
            // Snapshot is taken on INSERT in postgres::add_step.
            step_plugin_version: 0,
            embedded_job: None,
        });
    }

    // Trigger provenance: a trigger describes a job-creation condition
    // and has no work of its own, so it is terminal the instant the Job
    // exists — the firing trigger is `Completed`, its alternatives
    // `Skipped` (the branch not taken). Resolved BEFORE the readiness
    // pass so a downstream `steps.<trigger>.done` predicate sees the
    // fired trigger as `.done` and promotes in the same call. Skipped
    // when no registry is supplied: the pure helper path then leaves
    // the trigger `Ready` for a marker handler to complete, while the
    // create path resolves it here.
    if let Some(registry) = step_registry {
        resolve_triggers(registry, &mut steps, job_metadata, date_anchor);
    }
    // Open-time readiness pass: flip the subject-gated steps to Ready
    // (or provably-N/A steps to Skipped).
    reevaluate(spec, &mut steps, subject, job_metadata);
    steps
}

/// Resolve `auto-on-materialize` (trigger) steps to their terminal
/// status at Job open: the firing trigger becomes `Completed`, every
/// alternative becomes `Skipped`. A trigger names a job-creation
/// condition and carries no completion logic of its own, so it is
/// terminal the instant the Job exists — never transient Ready work an
/// executor or the dispatcher's marker handler picks up. This is the
/// honest form of "every trigger fires regardless of spawn cause": only
/// the trigger that actually fired is recorded as `Completed`.
///
/// Which trigger fired is read from the Job's `metadata.trigger_name`
/// (stamped by the `jobs.spawn` rule that opened the Job, or absent for
/// operator-opened Jobs): the trigger step whose own
/// `metadata.trigger_name` matches it fires. With no provenance — an
/// operator-opened Job, a sole trigger, or a name that matches none —
/// the first trigger fires (the compat fallback). Downstream predicates
/// that fan in from multiple triggers use `steps.a.done OR steps.b.done`
/// and `.done` is `Completed`-only, so a `Skipped` alternative correctly
/// does not satisfy them.
///
/// Trigger steps are identified by the `AutoOnMaterialize` completion
/// property, never the kind name (no-step-kind-match). Returns the
/// indices whose status changed, for the caller's `step.created`
/// emission.
fn resolve_triggers(
    registry: &StepRegistry,
    steps: &mut [Step],
    job_metadata: &serde_json::Value,
    date_anchor: Option<chrono::NaiveDate>,
) -> Vec<usize> {
    let trigger_idxs: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            registry
                .get(&s.kind)
                .is_some_and(|st| st.completion == Completion::AutoOnMaterialize)
        })
        .map(|(i, _)| i)
        .collect();
    if trigger_idxs.is_empty() {
        return Vec::new();
    }

    let job_trigger = job_metadata.get("trigger_name").and_then(|v| v.as_str());
    let firing = job_trigger
        .and_then(|name| {
            trigger_idxs.iter().copied().find(|&i| {
                steps[i]
                    .metadata
                    .get("trigger_name")
                    .and_then(|v| v.as_str())
                    == Some(name)
            })
        })
        .or_else(|| trigger_idxs.first().copied());

    let mut changed = Vec::new();
    for &i in &trigger_idxs {
        let next = if Some(i) == firing {
            StepStatus::Completed
        } else {
            StepStatus::Skipped
        };
        if steps[i].status != next {
            steps[i].status = next;
            if next == StepStatus::Completed {
                steps[i].completed_on = date_anchor;
            }
            changed.push(i);
        }
    }
    changed
}

/// Humanize a kebab-case slug into a display title: `mash-in` →
/// `Mash in`. Used when a `StepSpec` declares no `title_template`.
fn humanize_slug(slug: &str) -> String {
    let mut s = slug.replace('-', " ");
    if let Some(first) = s.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    s
}

/// The upstream step slugs a predicate reads — its `steps.<slug>.…`
/// paths — sorted + deduped. Powers the denormalized `blocked_by`
/// edge list and the satisfiability check in [`reevaluate`]. An
/// unparseable predicate contributes no refs; the viability lint is
/// what rejects those at author / publish / boot time.
pub fn predicate_step_refs(ready_when: &str) -> Vec<String> {
    let Ok(expr) = boss_expr::parse(ready_when) else {
        return Vec::new();
    };
    let mut slugs: Vec<String> = boss_expr::references(&expr)
        .into_iter()
        .filter(|path| path.len() >= 2 && path[0] == "steps")
        .map(|path| path[1].clone())
        .collect();
    slugs.sort();
    slugs.dedup();
    slugs
}

/// Build the predicate-evaluation payload for a Job's current state:
/// `{ subject, job: { metadata }, steps: { <slug>: { done, metadata } } }`.
/// `steps` is keyed by each `StepSpec.title` slug, paired to the live
/// `Step` by index (== `sort_order`).
fn build_context(
    spec: &JobKindSpec,
    steps: &[Step],
    subject: &Subject,
    job_metadata: &serde_json::Value,
) -> serde_json::Value {
    let mut steps_obj = serde_json::Map::new();
    for (spec_step, step) in spec.steps.iter().zip(steps) {
        steps_obj.insert(
            spec_step.title.clone(),
            serde_json::json!({
                "done": step.status == StepStatus::Completed,
                "metadata": step.metadata,
            }),
        );
    }
    serde_json::json!({
        "subject": serde_json::to_value(subject).unwrap_or(serde_json::Value::Null),
        "job": { "metadata": job_metadata },
        "steps": serde_json::Value::Object(steps_obj),
    })
}

/// Evaluate a `ready_when` predicate against a context payload.
/// `None` on a parse error, an eval error (e.g. a referenced metadata
/// field the upstream step hasn't set yet), or a non-boolean result.
/// JobKind predicates are pure boolean / field / comparison
/// expressions, so no helper functions are registered.
fn eval_ready_when(ready_when: &str, payload: &serde_json::Value) -> Option<bool> {
    let expr = boss_expr::parse(ready_when).ok()?;
    let ctx = boss_expr::Context {
        payload,
        helpers: &boss_expr::NoHelpers,
    };
    boss_expr::eval(&expr, &ctx).ok()?.as_bool()
}

/// True iff every step that step `idx`'s predicate references has
/// reached a terminal state (`Completed` / `Skipped`) — i.e. no
/// future change can flip its `ready_when`. Unknown refs (which the
/// lint forbids) count as terminal so a stray reference can't wedge a
/// step `Pending` forever.
fn refs_all_terminal(spec: &JobKindSpec, steps: &[Step], idx: usize) -> bool {
    let Some(spec_step) = spec.steps.get(idx) else {
        return true;
    };
    predicate_step_refs(&spec_step.ready_when)
        .iter()
        .all(|slug| {
            spec.steps
                .iter()
                .position(|s| &s.title == slug)
                .and_then(|j| steps.get(j))
                .map(|s| matches!(s.status, StepStatus::Completed | StepStatus::Skipped))
                .unwrap_or(true)
        })
}

/// Re-evaluate every `Pending` step against the current Job state,
/// promoting `Pending → Ready` when its `ready_when` is satisfied, or
/// `Pending → Skipped` when it is provably unsatisfiable (every step
/// it references is terminal and the predicate still won't hold).
/// Returns the indices whose status changed so the caller can emit
/// the matching `step.updated` events.
///
/// The single readiness engine shared by the live API (boss-jobs
/// http) and the simulator (which builds audit_log directly). Steps
/// must be in spec order (`sort_order == index`); the materializer
/// guarantees that. Iterates to a fixpoint so a `Skipped` cascade —
/// one skip making a downstream predicate's refs all-terminal —
/// settles in a single call. A spec/steps length mismatch (only
/// possible if a JobKind was republished mid-flight with a different
/// step count) is treated as "leave everything as-is."
pub fn reevaluate(
    spec: &JobKindSpec,
    steps: &mut [Step],
    subject: &Subject,
    job_metadata: &serde_json::Value,
) -> Vec<usize> {
    let mut changed = Vec::new();
    if spec.steps.len() != steps.len() {
        return changed;
    }
    loop {
        let ctx = build_context(spec, steps, subject, job_metadata);
        let mut moved = false;
        for idx in 0..steps.len() {
            if steps[idx].status != StepStatus::Pending {
                continue;
            }
            let next = match eval_ready_when(&spec.steps[idx].ready_when, &ctx) {
                Some(true) => Some(StepStatus::Ready),
                Some(false) | None => {
                    refs_all_terminal(spec, steps, idx).then_some(StepStatus::Skipped)
                }
            };
            if let Some(status) = next {
                steps[idx].status = status;
                changed.push(idx);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    changed
}

/// If the step has an `authority_role`, surface it in metadata so the
/// sign-off gate in `boss-jobs::http::update_step` can enforce it.
fn merge_metadata(defaults: &serde_json::Value, step: &StepSpec) -> serde_json::Value {
    let mut merged = match defaults {
        serde_json::Value::Object(_) => defaults.clone(),
        _ => serde_json::Value::Object(serde_json::Map::new()),
    };
    if let (Some(role), serde_json::Value::Object(m)) = (&step.authority_role, &mut merged) {
        m.insert(
            "authority_role".to_string(),
            serde_json::Value::String(role.clone()),
        );
    }
    merged
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum JobKindError {
    #[error("job kind not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid spec: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(String),
}

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

#[async_trait]
pub trait JobKindRegistry: Send + Sync {
    /// Return the currently-active spec for `kind`, or NotFound if
    /// no active row exists.
    async fn get_active(&self, kind: &str) -> Result<JobKindSpec, JobKindError>;

    /// Return a specific historical version. Version 0 is reserved
    /// as "latest active."
    async fn get_version(&self, kind: &str, version: i32) -> Result<JobKindSpec, JobKindError>;

    /// List every active spec, optionally filtered by category.
    async fn list_active(&self, category: Option<&str>) -> Result<Vec<JobKindSpec>, JobKindError>;

    /// Every version of a single kind, oldest first. Includes drafts
    /// and retired rows.
    async fn list_versions(&self, kind: &str) -> Result<Vec<JobKindSpec>, JobKindError>;

    /// Append a new version with `status = Draft`. If no prior rows
    /// exist, the new row is version 1; otherwise it's max(version)+1.
    /// Returns the stored spec (with its assigned version + created_at).
    async fn create_draft(&self, spec: JobKindSpec) -> Result<JobKindSpec, JobKindError>;

    /// Flip the latest draft row of this kind to active and any
    /// previous active row to retired. Transactional.
    async fn publish(&self, kind: &str) -> Result<JobKindSpec, JobKindError>;

    /// Flip the active row of this kind to retired. Idempotent if
    /// already retired.
    async fn retire(&self, kind: &str) -> Result<(), JobKindError>;

    /// Look up the active spec and materialize its step DAG against
    /// the given subject. Default impl fetches + delegates to the
    /// pure `materialize_steps` helper; adapters can override if a
    /// smarter path is available.
    ///
    /// The closure must be `Send` so the returned future is `Send`
    /// across the async trait boundary. Callers that need non-`Send`
    /// counters (e.g. the sim) call `materialize_steps` directly on
    /// a spec they fetched separately.
    async fn materialize_steps_for_kind(
        &self,
        kind: &str,
        subject: &Subject,
        job_id: JobId,
        job_metadata: &serde_json::Value,
        step_ids: &mut (dyn FnMut() -> StepId + Send),
    ) -> Result<Vec<Step>, JobKindError> {
        let spec = self.get_active(kind).await?;
        Ok(materialize_steps(
            &spec,
            subject,
            job_id,
            job_metadata,
            step_ids,
        ))
    }

    /// Single-shot create + publish, used by the `job-kind-publish`
    /// StepType's dispatch path inside `boss-jobs-api::update_step`.
    /// The meta-Job that authored the spec passes its own id as
    /// `authoring_job_id`; the row stamps `created_by =
    /// "job-{authoring_job_id}"` so the bootstrap reconciler
    /// preserves the row going forward (`created_by != 'bootstrap'`
    /// → operator-owned).
    ///
    /// Semantics, in one transaction:
    /// - Compute `next_version = max(version) + 1`, or 1 if no
    ///   prior rows for this kind.
    /// - INSERT the spec at that version with `status='active'`.
    /// - UPDATE any previously-active row of the same kind to
    ///   `status='retired'`.
    ///
    /// Returns the published spec with `version` + `created_at`
    /// reflecting the durable row. See
    /// `docs/architecture-decisions.md` §Jobs, JobKinds, Steps.
    async fn publish_authored(
        &self,
        spec: JobKindSpec,
        authoring_job_id: JobId,
    ) -> Result<JobKindSpec, JobKindError>;

    /// Reconcile the active rows in the registry against a set of
    /// platform-supplied defaults. For each default:
    ///
    /// - No row exists for `kind` → insert as version 1, active,
    ///   `created_by = 'bootstrap'`. Counted as `inserted`.
    /// - Active row exists, `created_by = 'bootstrap'`, body
    ///   drifted from the default (label / category /
    ///   subject_kinds / steps / metadata_schema /
    ///   entitlements / metadata / on_complete_create / owning_team) → upsert in
    ///   place, restamping `created_by = 'bootstrap'`. Counted as
    ///   `refreshed`. The version + created_at are preserved so a
    ///   bootstrap fixup never reads as a publish event.
    /// - Active row exists, `created_by != 'bootstrap'` → preserve
    ///   untouched. Counted as `preserved`.
    /// - Active row exists, `created_by = 'bootstrap'`, no drift →
    ///   no write. Counted as `unchanged`.
    ///
    /// Same semantic shape as `boss_policy_client::PolicyRepository::
    /// bootstrap_reconcile`. Used by
    /// `boss-jobs-api`'s startup loop to upsert `platform_kinds()`
    /// — including `job-kind-design`, the meta-kind that owns the
    /// design / review / publish workflow for every other kind in
    /// the registry.
    async fn bootstrap_reconcile(
        &self,
        defaults: &[JobKindSpec],
    ) -> Result<KindReconcileStats, JobKindError>;
}

/// Result of a `bootstrap_reconcile` call. Counts each branch so
/// the service log records how much drift was healed on this boot.
/// Same shape as `boss_policy_client::ReconcileStats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindReconcileStats {
    /// New rows inserted as `created_by = 'bootstrap'`.
    pub inserted: usize,
    /// Bootstrap-owned rows whose body was refreshed to match the
    /// current default.
    pub refreshed: usize,
    /// Operator-edited rows left untouched
    /// (`created_by != 'bootstrap'`).
    pub preserved: usize,
    /// Bootstrap-owned rows already matching the default — no
    /// write.
    pub unchanged: usize,
}

/// Helper: returns true iff every body field that bootstrap
/// reconcile cares about matches between `existing` and `default`.
/// Fields excluded from the comparison: `kind` (key), `version`
/// (preserved), `status` (always `Active` post-reconcile),
/// `created_at` (preserved), `description` (cosmetic — change in
/// description alone shouldn't trigger a refresh write),
/// `authoring_job_id` (the bootstrap-side default doesn't carry
/// one).
fn kind_body_matches(existing: &JobKindSpec, default: &JobKindSpec) -> bool {
    existing.label == default.label
        && existing.category == default.category
        && existing.subject_kinds == default.subject_kinds
        && existing.steps == default.steps
        && existing.metadata_schema == default.metadata_schema
        && existing.entitlements == default.entitlements
        && existing.metadata == default.metadata
        && existing.on_complete_create == default.on_complete_create
        && existing.owning_team == default.owning_team
}

// ---------------------------------------------------------------------------
// In-memory adapter
// ---------------------------------------------------------------------------

/// Mutex-backed in-memory registry. Every async fn resolves immediately;
/// safe to call from either a tokio or a non-tokio context.
pub struct InMemoryJobKinds {
    rows: Arc<Mutex<HashMap<(String, i32), JobKindSpec>>>,
    /// Tracks which rows came from a bootstrap reconcile. Mirrors
    /// the `created_by = 'bootstrap'` discriminator the postgres
    /// adapter uses, so reconcile semantics match across adapters
    /// in tests.
    bootstrap_owned: Arc<Mutex<std::collections::HashSet<(String, i32)>>>,
}

impl Default for InMemoryJobKinds {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryJobKinds {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_owned: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Seed helper for tests + bootstrap. Inserts a row as-is without
    /// versioning logic — used for the seed migration. The seeded
    /// row is NOT marked bootstrap-owned (use `seed_bootstrap` for
    /// that semantic).
    pub fn seed(&self, spec: JobKindSpec) -> Result<(), JobKindError> {
        let mut rows = self.rows.lock().unwrap();
        if rows.contains_key(&(spec.kind.clone(), spec.version)) {
            return Err(JobKindError::Conflict(format!(
                "row already exists: {}@{}",
                spec.kind, spec.version
            )));
        }
        rows.insert((spec.kind.clone(), spec.version), spec);
        Ok(())
    }

    fn snapshot(&self) -> Vec<JobKindSpec> {
        let rows = self.rows.lock().unwrap();
        rows.values().cloned().collect()
    }

    fn max_version(&self, kind: &str) -> Option<i32> {
        let rows = self.rows.lock().unwrap();
        rows.keys()
            .filter(|(k, _)| k == kind)
            .map(|(_, v)| *v)
            .max()
    }
}

#[async_trait]
impl JobKindRegistry for InMemoryJobKinds {
    async fn get_active(&self, kind: &str) -> Result<JobKindSpec, JobKindError> {
        let rows = self.snapshot();
        rows.into_iter()
            .find(|r| r.kind == kind && r.status == JobKindStatus::Active)
            .ok_or_else(|| JobKindError::NotFound(format!("no active kind: {kind}")))
    }

    async fn get_version(&self, kind: &str, version: i32) -> Result<JobKindSpec, JobKindError> {
        let rows = self.rows.lock().unwrap();
        rows.get(&(kind.to_string(), version))
            .cloned()
            .ok_or_else(|| JobKindError::NotFound(format!("{kind}@v{version}")))
    }

    async fn list_active(&self, category: Option<&str>) -> Result<Vec<JobKindSpec>, JobKindError> {
        let mut rows: Vec<JobKindSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.status == JobKindStatus::Active)
            .filter(|r| category.is_none_or(|c| r.category == c))
            .collect();
        rows.sort_by(|a, b| a.kind.cmp(&b.kind));
        Ok(rows)
    }

    async fn list_versions(&self, kind: &str) -> Result<Vec<JobKindSpec>, JobKindError> {
        let mut rows: Vec<JobKindSpec> = self
            .snapshot()
            .into_iter()
            .filter(|r| r.kind == kind)
            .collect();
        rows.sort_by_key(|r| r.version);
        Ok(rows)
    }

    async fn create_draft(&self, mut spec: JobKindSpec) -> Result<JobKindSpec, JobKindError> {
        let next = self.max_version(&spec.kind).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = JobKindStatus::Draft;
        spec.created_at = Utc::now();
        let mut rows = self.rows.lock().unwrap();
        rows.insert((spec.kind.clone(), spec.version), spec.clone());
        Ok(spec)
    }

    async fn publish(&self, kind: &str) -> Result<JobKindSpec, JobKindError> {
        let mut rows = self.rows.lock().unwrap();

        // Find the latest draft for this kind.
        let latest_draft = rows
            .values()
            .filter(|r| r.kind == kind && r.status == JobKindStatus::Draft)
            .max_by_key(|r| r.version)
            .cloned()
            .ok_or_else(|| {
                JobKindError::NotFound(format!("no draft to publish for kind: {kind}"))
            })?;

        // Demote any currently-active row for this kind.
        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == JobKindStatus::Active {
                row.status = JobKindStatus::Retired;
            }
        }

        // Promote the draft.
        let key = (latest_draft.kind.clone(), latest_draft.version);
        let row = rows.get_mut(&key).unwrap();
        row.status = JobKindStatus::Active;
        Ok(row.clone())
    }

    async fn retire(&self, kind: &str) -> Result<(), JobKindError> {
        let mut rows = self.rows.lock().unwrap();
        let any_active = rows
            .values()
            .any(|r| r.kind == kind && r.status == JobKindStatus::Active);
        if !any_active {
            // Idempotent — nothing to do.
            return Ok(());
        }
        for ((k, _), row) in rows.iter_mut() {
            if k == kind && row.status == JobKindStatus::Active {
                row.status = JobKindStatus::Retired;
            }
        }
        Ok(())
    }

    async fn publish_authored(
        &self,
        mut spec: JobKindSpec,
        authoring_job_id: JobId,
    ) -> Result<JobKindSpec, JobKindError> {
        let next = self.max_version(&spec.kind).unwrap_or(0) + 1;
        spec.version = next;
        spec.status = JobKindStatus::Active;
        spec.created_at = Utc::now();
        spec.authoring_job_id = Some(*authoring_job_id.inner().as_uuid());

        let key = (spec.kind.clone(), spec.version);
        let mut rows = self.rows.lock().unwrap();
        // Retire any currently-active row of the same kind.
        for ((k, _), row) in rows.iter_mut() {
            if k == &spec.kind && row.status == JobKindStatus::Active {
                row.status = JobKindStatus::Retired;
            }
        }
        rows.insert(key.clone(), spec.clone());

        // The new row was published via a Job — operator-owned, NOT
        // bootstrap. Make sure the discriminator reflects that so
        // future reconciles preserve it.
        let mut owned = self.bootstrap_owned.lock().unwrap();
        owned.remove(&key);

        Ok(spec)
    }

    async fn bootstrap_reconcile(
        &self,
        defaults: &[JobKindSpec],
    ) -> Result<KindReconcileStats, JobKindError> {
        let mut stats = KindReconcileStats::default();
        let mut rows = self.rows.lock().unwrap();
        let mut owned = self.bootstrap_owned.lock().unwrap();

        for default in defaults {
            // Find the active row for this kind, if any.
            let active_key: Option<(String, i32)> = rows
                .iter()
                .find(|((k, _), r)| k == &default.kind && r.status == JobKindStatus::Active)
                .map(|((k, v), _)| (k.clone(), *v));

            match active_key {
                None => {
                    // No active row — insert as v1, active,
                    // bootstrap-owned. Force version=1 + status=Active
                    // even if the default carried different values, so
                    // a malformed default can't sneak a draft into the
                    // active slot.
                    let mut spec = default.clone();
                    spec.version = 1;
                    spec.status = JobKindStatus::Active;
                    let key = (spec.kind.clone(), spec.version);
                    rows.insert(key.clone(), spec);
                    owned.insert(key);
                    stats.inserted += 1;
                }
                Some(key) => {
                    let existing = rows.get(&key).expect("just located it").clone();
                    if owned.contains(&key) {
                        if !kind_body_matches(&existing, default) {
                            // Refresh the body in place. Preserve
                            // version + created_at so a fixup never
                            // looks like a new publish.
                            let mut updated = default.clone();
                            updated.version = existing.version;
                            updated.status = JobKindStatus::Active;
                            updated.created_at = existing.created_at;
                            rows.insert(key, updated);
                            stats.refreshed += 1;
                        } else {
                            stats.unchanged += 1;
                        }
                    } else {
                        // Operator-owned — preserve untouched.
                        stats.preserved += 1;
                    }
                }
            }
        }

        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// Postgres adapter
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
mod pg {
    use super::*;
    use sqlx::PgPool;

    pub struct PgJobKinds {
        pool: PgPool,
    }

    impl PgJobKinds {
        pub fn new(pool: PgPool) -> Self {
            Self { pool }
        }
    }

    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        version: i32,
        status: String,
        label: String,
        description: Option<String>,
        category: String,
        subject_kinds: serde_json::Value,
        steps: serde_json::Value,
        metadata_schema: serde_json::Value,
        entitlements: serde_json::Value,
        metadata: serde_json::Value,
        on_complete_create: serde_json::Value,
        owning_team: String,
        authoring_job_id: Option<Uuid>,
        created_at: DateTime<Utc>,
    }

    fn row_to_spec(r: Row) -> Result<JobKindSpec, JobKindError> {
        let subject_kinds: Vec<String> = serde_json::from_value(r.subject_kinds)
            .map_err(|e| JobKindError::Storage(format!("subject_kinds decode: {e}")))?;
        let steps: Vec<StepSpec> = serde_json::from_value(r.steps)
            .map_err(|e| JobKindError::Storage(format!("steps decode: {e}")))?;
        let on_complete_create: Vec<JobTrigger> = serde_json::from_value(r.on_complete_create)
            .map_err(|e| JobKindError::Storage(format!("on_complete_create decode: {e}")))?;
        let status = r
            .status
            .parse::<JobKindStatus>()
            .map_err(JobKindError::Storage)?;
        Ok(JobKindSpec {
            kind: r.kind,
            version: r.version,
            status,
            label: r.label,
            description: r.description,
            category: r.category,
            subject_kinds,
            steps,
            metadata_schema: r.metadata_schema,
            entitlements: r.entitlements,
            metadata: r.metadata,
            on_complete_create,
            owning_team: r.owning_team,
            authoring_job_id: r.authoring_job_id,
            created_at: r.created_at,
        })
    }

    #[async_trait]
    impl JobKindRegistry for PgJobKinds {
        async fn get_active(&self, kind: &str) -> Result<JobKindSpec, JobKindError> {
            let row: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM job_kinds
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| JobKindError::NotFound(format!("no active kind: {kind}")))
        }

        async fn get_version(&self, kind: &str, version: i32) -> Result<JobKindSpec, JobKindError> {
            let row: Option<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM job_kinds
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(version)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;
            row.map(row_to_spec)
                .transpose()?
                .ok_or_else(|| JobKindError::NotFound(format!("{kind}@v{version}")))
        }

        async fn list_active(
            &self,
            category: Option<&str>,
        ) -> Result<Vec<JobKindSpec>, JobKindError> {
            let rows: Vec<Row> = match category {
                Some(c) => {
                    sqlx::query_as(
                        "SELECT kind, version, status, label, description, category,
                            subject_kinds, steps, metadata_schema, entitlements, metadata,
                            on_complete_create, owning_team, authoring_job_id, created_at
                     FROM job_kinds
                     WHERE status = 'active' AND category = $1
                     ORDER BY kind",
                    )
                    .bind(c)
                    .fetch_all(&self.pool)
                    .await
                }
                None => {
                    sqlx::query_as(
                        "SELECT kind, version, status, label, description, category,
                            subject_kinds, steps, metadata_schema, entitlements, metadata,
                            on_complete_create, owning_team, authoring_job_id, created_at
                     FROM job_kinds
                     WHERE status = 'active'
                     ORDER BY kind",
                    )
                    .fetch_all(&self.pool)
                    .await
                }
            }
            .map_err(|e| JobKindError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn list_versions(&self, kind: &str) -> Result<Vec<JobKindSpec>, JobKindError> {
            let rows: Vec<Row> = sqlx::query_as(
                "SELECT kind, version, status, label, description, category,
                        subject_kinds, steps, metadata_schema, entitlements, metadata,
                        on_complete_create, owning_team, authoring_job_id, created_at
                 FROM job_kinds
                 WHERE kind = $1
                 ORDER BY version",
            )
            .bind(kind)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;
            rows.into_iter().map(row_to_spec).collect()
        }

        async fn create_draft(&self, mut spec: JobKindSpec) -> Result<JobKindSpec, JobKindError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;

            // Next version = max(version) + 1, or 1 if no rows. `MAX(...)`
            // returns a single row with a NULL column when the filter
            // matches nothing, so decode as `Option<i32>`.
            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM job_kinds WHERE kind = $1")
                    .bind(&spec.kind)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| JobKindError::Storage(e.to_string()))?;
            let next = max.0.map(|v| v + 1).unwrap_or(1);

            spec.version = next;
            spec.status = JobKindStatus::Draft;
            spec.created_at = Utc::now();

            let subject_kinds_json = serde_json::to_value(&spec.subject_kinds)
                .map_err(|e| JobKindError::Invalid(e.to_string()))?;
            let steps_json = serde_json::to_value(&spec.steps)
                .map_err(|e| JobKindError::Invalid(e.to_string()))?;
            let on_complete_create_json = serde_json::to_value(&spec.on_complete_create)
                .map_err(|e| JobKindError::Invalid(e.to_string()))?;

            sqlx::query(
                "INSERT INTO job_kinds
                    (kind, version, status, label, description, category,
                     subject_kinds, steps, metadata_schema, entitlements, metadata,
                     on_complete_create, owning_team, authoring_job_id, created_at)
                 VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&subject_kinds_json)
            .bind(&steps_json)
            .bind(&spec.metadata_schema)
            .bind(&spec.entitlements)
            .bind(&spec.metadata)
            .bind(&on_complete_create_json)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;
            Ok(spec)
        }

        async fn publish(&self, kind: &str) -> Result<JobKindSpec, JobKindError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;

            // Pick the latest draft.
            let draft_version: Option<(i32,)> = sqlx::query_as(
                "SELECT version FROM job_kinds
                 WHERE kind = $1 AND status = 'draft'
                 ORDER BY version DESC LIMIT 1",
            )
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;

            let draft_version = draft_version
                .map(|(v,)| v)
                .ok_or_else(|| JobKindError::NotFound(format!("no draft to publish: {kind}")))?;

            // Retire any currently-active row.
            sqlx::query(
                "UPDATE job_kinds SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;

            // Promote the draft.
            sqlx::query(
                "UPDATE job_kinds SET status = 'active'
                 WHERE kind = $1 AND version = $2",
            )
            .bind(kind)
            .bind(draft_version)
            .execute(&mut *tx)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;

            self.get_version(kind, draft_version).await
        }

        async fn retire(&self, kind: &str) -> Result<(), JobKindError> {
            sqlx::query(
                "UPDATE job_kinds SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(kind)
            .execute(&self.pool)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;
            Ok(())
        }

        async fn publish_authored(
            &self,
            mut spec: JobKindSpec,
            authoring_job_id: JobId,
        ) -> Result<JobKindSpec, JobKindError> {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;

            // Compute next version inside the transaction so a
            // concurrent publish can't race us into a duplicate.
            let max: (Option<i32>,) =
                sqlx::query_as("SELECT MAX(version) FROM job_kinds WHERE kind = $1")
                    .bind(&spec.kind)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| JobKindError::Storage(e.to_string()))?;
            let next = max.0.map(|v| v + 1).unwrap_or(1);

            spec.version = next;
            spec.status = JobKindStatus::Active;
            spec.created_at = Utc::now();
            spec.authoring_job_id = Some(*authoring_job_id.inner().as_uuid());

            // Retire any existing active row of this kind first —
            // the partial unique index demands at most one Active.
            sqlx::query(
                "UPDATE job_kinds SET status = 'retired'
                 WHERE kind = $1 AND status = 'active'",
            )
            .bind(&spec.kind)
            .execute(&mut *tx)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;

            let subject_kinds_json = serde_json::to_value(&spec.subject_kinds)
                .map_err(|e| JobKindError::Invalid(e.to_string()))?;
            let steps_json = serde_json::to_value(&spec.steps)
                .map_err(|e| JobKindError::Invalid(e.to_string()))?;
            let on_complete_create_json = serde_json::to_value(&spec.on_complete_create)
                .map_err(|e| JobKindError::Invalid(e.to_string()))?;

            // Stamp created_by = "job-<authoring_job_id>" so the
            // bootstrap reconciler preserves this row (operator-
            // owned). Same shape any future reconcile decision uses.
            let created_by = format!("job-{}", authoring_job_id);

            sqlx::query(
                "INSERT INTO job_kinds
                    (kind, version, status, label, description, category,
                     subject_kinds, steps, metadata_schema, entitlements, metadata,
                     on_complete_create, owning_team, authoring_job_id,
                     created_by, created_at)
                 VALUES ($1, $2, 'active', $3, $4, $5, $6, $7, $8, $9, $10, $11,
                         $12, $13, $14, $15)",
            )
            .bind(&spec.kind)
            .bind(spec.version)
            .bind(&spec.label)
            .bind(&spec.description)
            .bind(&spec.category)
            .bind(&subject_kinds_json)
            .bind(&steps_json)
            .bind(&spec.metadata_schema)
            .bind(&spec.entitlements)
            .bind(&spec.metadata)
            .bind(&on_complete_create_json)
            .bind(&spec.owning_team)
            .bind(spec.authoring_job_id)
            .bind(&created_by)
            .bind(spec.created_at)
            .execute(&mut *tx)
            .await
            .map_err(|e| JobKindError::Storage(e.to_string()))?;

            tx.commit()
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;

            Ok(spec)
        }

        async fn bootstrap_reconcile(
            &self,
            defaults: &[JobKindSpec],
        ) -> Result<KindReconcileStats, JobKindError> {
            // Dedicated row shape that includes the bootstrap
            // discriminator alongside the body. The non-reconcile
            // read paths above use `Row` (which doesn't carry
            // `created_by`) so reconcile is the only caller that
            // needs this struct.
            #[derive(sqlx::FromRow)]
            struct ReconcileRow {
                kind: String,
                version: i32,
                status: String,
                label: String,
                description: Option<String>,
                category: String,
                subject_kinds: serde_json::Value,
                steps: serde_json::Value,
                metadata_schema: serde_json::Value,
                entitlements: serde_json::Value,
                metadata: serde_json::Value,
                on_complete_create: serde_json::Value,
                owning_team: String,
                authoring_job_id: Option<Uuid>,
                created_at: DateTime<Utc>,
                created_by: String,
            }

            let mut stats = KindReconcileStats::default();

            for default in defaults {
                let row: Option<ReconcileRow> = sqlx::query_as(
                    "SELECT kind, version, status, label, description, category,
                            subject_kinds, steps, metadata_schema, entitlements, metadata,
                            on_complete_create, owning_team, authoring_job_id, created_at,
                            created_by
                     FROM job_kinds
                     WHERE kind = $1 AND status = 'active'",
                )
                .bind(&default.kind)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| JobKindError::Storage(e.to_string()))?;

                match row {
                    None => {
                        // Insert as v1, active, bootstrap-owned.
                        let subject_kinds_json = serde_json::to_value(&default.subject_kinds)
                            .map_err(|e| JobKindError::Invalid(e.to_string()))?;
                        let steps_json = serde_json::to_value(&default.steps)
                            .map_err(|e| JobKindError::Invalid(e.to_string()))?;
                        let on_complete_create_json =
                            serde_json::to_value(&default.on_complete_create)
                                .map_err(|e| JobKindError::Invalid(e.to_string()))?;

                        sqlx::query(
                            "INSERT INTO job_kinds
                                (kind, version, status, label, description, category,
                                 subject_kinds, steps, metadata_schema, entitlements, metadata,
                                 on_complete_create, owning_team, authoring_job_id,
                                 created_by, created_at)
                             VALUES ($1, 1, 'active', $2, $3, $4, $5, $6, $7, $8, $9,
                                     $10, $11, $12, 'bootstrap', NOW())",
                        )
                        .bind(&default.kind)
                        .bind(&default.label)
                        .bind(&default.description)
                        .bind(&default.category)
                        .bind(&subject_kinds_json)
                        .bind(&steps_json)
                        .bind(&default.metadata_schema)
                        .bind(&default.entitlements)
                        .bind(&default.metadata)
                        .bind(&on_complete_create_json)
                        .bind(&default.owning_team)
                        .bind(default.authoring_job_id)
                        .execute(&self.pool)
                        .await
                        .map_err(|e| JobKindError::Storage(e.to_string()))?;

                        stats.inserted += 1;
                    }
                    Some(reconcile_row) => {
                        let version = reconcile_row.version;
                        let created_by = reconcile_row.created_by.clone();
                        let body = Row {
                            kind: reconcile_row.kind,
                            version: reconcile_row.version,
                            status: reconcile_row.status,
                            label: reconcile_row.label,
                            description: reconcile_row.description,
                            category: reconcile_row.category,
                            subject_kinds: reconcile_row.subject_kinds,
                            steps: reconcile_row.steps,
                            metadata_schema: reconcile_row.metadata_schema,
                            entitlements: reconcile_row.entitlements,
                            metadata: reconcile_row.metadata,
                            on_complete_create: reconcile_row.on_complete_create,
                            owning_team: reconcile_row.owning_team,
                            authoring_job_id: reconcile_row.authoring_job_id,
                            created_at: reconcile_row.created_at,
                        };
                        let existing = row_to_spec(body)?;
                        if created_by == "bootstrap" {
                            if !kind_body_matches(&existing, default) {
                                // Refresh body in place. Preserve
                                // version + created_at; restamp
                                // created_by = 'bootstrap' so the
                                // discriminator stays accurate.
                                let subject_kinds_json =
                                    serde_json::to_value(&default.subject_kinds)
                                        .map_err(|e| JobKindError::Invalid(e.to_string()))?;
                                let steps_json = serde_json::to_value(&default.steps)
                                    .map_err(|e| JobKindError::Invalid(e.to_string()))?;
                                let on_complete_create_json =
                                    serde_json::to_value(&default.on_complete_create)
                                        .map_err(|e| JobKindError::Invalid(e.to_string()))?;

                                sqlx::query(
                                    "UPDATE job_kinds SET
                                        label = $3,
                                        description = $4,
                                        category = $5,
                                        subject_kinds = $6,
                                        steps = $7,
                                        metadata_schema = $8,
                                        entitlements = $9,
                                        metadata = $10,
                                        on_complete_create = $11,
                                        owning_team = $12,
                                        created_by = 'bootstrap'
                                     WHERE kind = $1 AND version = $2",
                                )
                                .bind(&default.kind)
                                .bind(version)
                                .bind(&default.label)
                                .bind(&default.description)
                                .bind(&default.category)
                                .bind(&subject_kinds_json)
                                .bind(&steps_json)
                                .bind(&default.metadata_schema)
                                .bind(&default.entitlements)
                                .bind(&default.metadata)
                                .bind(&on_complete_create_json)
                                .bind(&default.owning_team)
                                .execute(&self.pool)
                                .await
                                .map_err(|e| JobKindError::Storage(e.to_string()))?;

                                stats.refreshed += 1;
                            } else {
                                stats.unchanged += 1;
                            }
                        } else {
                            stats.preserved += 1;
                        }
                    }
                }
            }

            Ok(stats)
        }
    }
}

#[cfg(feature = "postgres")]
pub use pg::PgJobKinds;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_metadata_substitutes_subject_fields_in_string_leaves() {
        let subject = Subject::new("account", "acc-bigseed-0042");
        let template = serde_json::json!({
            "account_id": "{subject.id}",
            "amount_cents": 420000,
            "currency": "USD",
            "line_items": [
                { "amount_cents": 240000, "description": "Pale Ale" },
                { "amount_cents": 180000, "description": "IPA" }
            ]
        });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(
            expanded.get("account_id").and_then(|v| v.as_str()),
            Some("acc-bigseed-0042"),
            "subject.id should substitute into account_id"
        );
        // Non-template strings pass through verbatim.
        assert_eq!(
            expanded.get("currency").and_then(|v| v.as_str()),
            Some("USD")
        );
        // Non-string leaves pass through.
        assert_eq!(
            expanded.get("amount_cents").and_then(|v| v.as_i64()),
            Some(420000)
        );
        // Recursion through arrays + nested objects works.
        let lines = expanded
            .get("line_items")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0].get("description").and_then(|v| v.as_str()),
            Some("Pale Ale")
        );
    }

    #[test]
    fn expand_metadata_handles_compound_template_strings() {
        // The bridge handlers accept `"PO-{subject.id}"` as the
        // po_id default — substitution must work mid-string, not
        // only when the entire value is a single token.
        let subject = Subject::new("vendor", "vnd-malt-001");
        let template = serde_json::json!({ "po_id": "PO-{subject.id}" });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(
            expanded.get("po_id").and_then(|v| v.as_str()),
            Some("PO-vnd-malt-001")
        );
    }

    #[test]
    fn expand_metadata_leaves_unknown_tokens_alone() {
        // No `{subject.X}` token = no rewrite. Unknown tokens
        // (e.g. `{job.id}` — a future syntax) pass through
        // verbatim so no false substitutions sneak in.
        let subject = Subject::new("account", "acc-1");
        let template = serde_json::json!({
            "literal": "literal value with no token",
            "future": "something {job.id} that we don't expand yet",
        });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(
            expanded.get("literal").and_then(|v| v.as_str()),
            Some("literal value with no token")
        );
        assert_eq!(
            expanded.get("future").and_then(|v| v.as_str()),
            Some("something {job.id} that we don't expand yet")
        );
    }

    #[test]
    fn expand_metadata_substitutes_day_tokens_when_anchor_given() {
        let subject = Subject::new("location", "loc-brewery-brewhouse");
        let template = serde_json::json!({
            "period_start": "{day_minus_13}",
            "period_end": "{day}",
            "run_date": "{day_plus_1}",
        });
        let day = chrono::NaiveDate::from_ymd_opt(2025, 4, 14).unwrap();
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), Some(day));
        assert_eq!(expanded["period_start"], "2025-04-01");
        assert_eq!(expanded["period_end"], "2025-04-14");
        assert_eq!(expanded["run_date"], "2025-04-15");
    }

    #[test]
    fn expand_metadata_leaves_day_tokens_literal_when_anchor_none() {
        // Live-API callers without a clock get literal pass-through.
        // The downstream handler will then error on the malformed
        // date — a loud failure beats a silent wallclock stamp.
        let subject = Subject::new("location", "loc-1");
        let template = serde_json::json!({
            "period_start": "{day_minus_13}",
        });
        let expanded = expand_metadata(&template, &subject, &serde_json::json!({}), None);
        assert_eq!(expanded["period_start"], "{day_minus_13}");
    }

    #[test]
    fn job_trigger_round_trips_through_json_with_defaults() {
        // Empty `on_complete_create` is the common case and must
        // not bloat the wire/DB shape — `skip_serializing_if` keeps
        // it absent from output rather than serializing `[]`.
        let mut spec = JobKindSpec::platform_seed(
            "wholesale-order",
            "Wholesale Order",
            "sales",
            vec!["account".into()],
            Vec::new(),
        );
        let empty_json = serde_json::to_value(&spec).unwrap();
        assert!(empty_json.get("on_complete_create").is_none());

        // With a real trigger: a wholesale-order Job spawning an
        // invoice Job on the same Subject. Round-trips through JSON
        // including the default subject_source.
        spec.on_complete_create = vec![JobTrigger {
            kind: "invoice".to_string(),
            subject_source: "same".to_string(),
            metadata_seed: serde_json::json!({"source_kind": "wholesale-order"}),
        }];
        let with_trigger = serde_json::to_value(&spec).unwrap();
        assert_eq!(with_trigger["on_complete_create"][0]["kind"], "invoice");
        assert_eq!(
            with_trigger["on_complete_create"][0]["subject_source"],
            "same"
        );

        // Missing subject_source decodes to "same" via the default.
        let json_without_source = serde_json::json!({
            "kind": "invoice",
            "metadata_seed": {}
        });
        let trigger: JobTrigger = serde_json::from_value(json_without_source).unwrap();
        assert_eq!(trigger.subject_source, "same");
    }

    fn seed_spec(kind: &str) -> JobKindSpec {
        JobKindSpec::platform_seed(
            kind,
            format!("Test {kind}"),
            "test",
            vec!["asset".into()],
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn create_draft_assigns_next_version() {
        let reg = InMemoryJobKinds::new();
        let v1 = reg.create_draft(seed_spec("repair")).await.unwrap();
        assert_eq!(v1.version, 1);
        assert_eq!(v1.status, JobKindStatus::Draft);

        let v2 = reg.create_draft(seed_spec("repair")).await.unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(v2.status, JobKindStatus::Draft);
    }

    #[tokio::test]
    async fn publish_promotes_draft_and_retires_previous_active() {
        let reg = InMemoryJobKinds::new();

        // v1 drafted and published.
        reg.create_draft(seed_spec("repair")).await.unwrap();
        let active_v1 = reg.publish("repair").await.unwrap();
        assert_eq!(active_v1.version, 1);
        assert_eq!(active_v1.status, JobKindStatus::Active);

        // v2 drafted and published.
        reg.create_draft(seed_spec("repair")).await.unwrap();
        let active_v2 = reg.publish("repair").await.unwrap();
        assert_eq!(active_v2.version, 2);
        assert_eq!(active_v2.status, JobKindStatus::Active);

        // v1 now retired; only v2 is active.
        let v1 = reg.get_version("repair", 1).await.unwrap();
        assert_eq!(v1.status, JobKindStatus::Retired);
        let current = reg.get_active("repair").await.unwrap();
        assert_eq!(current.version, 2);
    }

    #[tokio::test]
    async fn retire_flips_active_to_retired() {
        let reg = InMemoryJobKinds::new();
        reg.create_draft(seed_spec("repair")).await.unwrap();
        reg.publish("repair").await.unwrap();

        reg.retire("repair").await.unwrap();
        let err = reg.get_active("repair").await.unwrap_err();
        match err {
            JobKindError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retire_is_idempotent() {
        let reg = InMemoryJobKinds::new();
        // Retiring a kind with no active row is a no-op, not an error.
        reg.retire("never-existed").await.unwrap();
        reg.retire("never-existed").await.unwrap();
    }

    #[tokio::test]
    async fn publish_without_draft_returns_not_found() {
        let reg = InMemoryJobKinds::new();
        let err = reg.publish("repair").await.unwrap_err();
        match err {
            JobKindError::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_active_filters_by_category() {
        let reg = InMemoryJobKinds::new();
        let mut refurb = seed_spec("refurb");
        refurb.category = "refurb".into();
        reg.seed(JobKindSpec {
            status: JobKindStatus::Active,
            ..refurb
        })
        .unwrap();

        let mut sale = seed_spec("sale");
        sale.category = "sales".into();
        reg.seed(JobKindSpec {
            status: JobKindStatus::Active,
            ..sale
        })
        .unwrap();

        let all = reg.list_active(None).await.unwrap();
        assert_eq!(all.len(), 2);

        let sales_only = reg.list_active(Some("sales")).await.unwrap();
        assert_eq!(sales_only.len(), 1);
        assert_eq!(sales_only[0].kind, "sale");
    }

    #[test]
    fn materialize_expands_title_template_and_sets_blocked_by() {
        // v2: the DAG is implicit in `ready_when`. `intake` is the
        // trigger; `diagnosis` + `parts-pull` both depend on it
        // (recovered into `blocked_by` from their predicates) and are
        // terminals so the spec is viable.
        let spec = JobKindSpec::platform_seed(
            "repair",
            "Repair",
            "service",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "intake".into(),
                    kind: "intake".into(),
                    ready_when: "true".into(),
                    title_template: "Intake {subject.id}".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "diagnosis".into(),
                    kind: "diagnosis".into(),
                    ready_when: "steps.intake.done".into(),
                    title_template: "Diagnose".into(),
                    terminal: Some(Terminal {
                        outcome: "diagnosed".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "parts-pull".into(),
                    kind: "parts-pull".into(),
                    ready_when: "steps.intake.done".into(),
                    title_template: "Parts check".into(),
                    terminal: Some(Terminal {
                        outcome: "pulled".into(),
                    }),
                    ..Default::default()
                },
            ],
        );

        let subject = Subject::new("asset", "SYS-42");
        let job_id = JobId::new();
        let job_metadata = serde_json::Value::Object(Default::default());

        let mut counter = 0u32;
        let steps = materialize_steps(&spec, &subject, job_id, &job_metadata, || {
            counter += 1;
            let raw = Uuid::from_u128(counter as u128);
            StepId::from_uuid(raw)
        });

        assert_eq!(steps.len(), 3);

        // Trigger intake: title expanded, no deps, Ready at open.
        assert_eq!(steps[0].kind, "intake");
        assert_eq!(steps[0].title, "Intake SYS-42");
        assert!(steps[0].blocked_by.is_empty());
        assert_eq!(steps[0].status, StepStatus::Ready);

        // Dependents: both blocked_by the trigger step, Pending at open.
        assert_eq!(steps[1].kind, "diagnosis");
        assert_eq!(steps[1].blocked_by, vec![steps[0].id]);
        assert_eq!(steps[1].status, StepStatus::Pending);
        assert_eq!(steps[2].kind, "parts-pull");
        assert_eq!(steps[2].blocked_by, vec![steps[0].id]);

        // job_id threaded through.
        for s in &steps {
            assert_eq!(s.job_id, job_id);
        }
    }

    #[test]
    fn materialize_surfaces_authority_role_into_metadata() {
        // Single-step JobKind: the one step is both the trigger and
        // the terminal (viable). authority_role must be surfaced into
        // step metadata so the sign-off gate can enforce it.
        let spec = JobKindSpec::platform_seed(
            "cert",
            "Certification",
            "service",
            vec!["asset".into()],
            vec![StepSpec {
                title: "annual".into(),
                kind: "sign-off".into(),
                ready_when: "true".into(),
                title_template: "Annual".into(),
                sign_offs_required: vec!["platform-admin".into()],
                authority_role: Some("qa-lead".into()),
                terminal: Some(Terminal {
                    outcome: "certified".into(),
                }),
                ..Default::default()
            }],
        );
        let subject = Subject::new("asset", "SYS-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut i = 0u32;
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, || {
            i += 1;
            StepId::from_uuid(Uuid::from_u128(i as u128))
        });
        assert_eq!(
            steps[0]
                .metadata
                .get("authority_role")
                .and_then(|v| v.as_str()),
            Some("qa-lead"),
            "authority_role must land in step metadata so the sign-off gate picks it up"
        );
        assert_eq!(
            steps[0].sign_offs_required,
            vec!["platform-admin".to_string()]
        );
    }

    #[tokio::test]
    async fn list_versions_returns_oldest_first() {
        let reg = InMemoryJobKinds::new();
        reg.create_draft(seed_spec("repair")).await.unwrap();
        reg.publish("repair").await.unwrap();
        reg.create_draft(seed_spec("repair")).await.unwrap();
        reg.publish("repair").await.unwrap();
        reg.create_draft(seed_spec("repair")).await.unwrap();

        let versions = reg.list_versions("repair").await.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version, 1);
        assert_eq!(versions[0].status, JobKindStatus::Retired);
        assert_eq!(versions[1].version, 2);
        assert_eq!(versions[1].status, JobKindStatus::Active);
        assert_eq!(versions[2].version, 3);
        assert_eq!(versions[2].status, JobKindStatus::Draft);
    }

    // ----- v2 conditional skip: predicate-driven Skipped status -----
    //
    // v1 modeled "this tier doesn't apply to this subject" by omitting
    // the tier from materialization (`skip_when` on TierSpec). v2 makes
    // every step materialize eagerly and uses `ready_when` + the
    // re-evaluator: a branch not taken becomes `Skipped` once its
    // referenced steps are all terminal. These tests pin that behavior.

    /// A fork JobKind: `decide` (trigger) routes to either `ship` or
    /// `scrap` based on `decide`'s `outcome` metadata. Both branches
    /// are terminals, so the spec is viable.
    fn fork_spec() -> JobKindSpec {
        JobKindSpec::platform_seed(
            "fork-job",
            "Fork Job",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "decide".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "ship".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.outcome = \"approved\"".into(),
                    terminal: Some(Terminal {
                        outcome: "shipped".into(),
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "scrap".into(),
                    kind: "task".into(),
                    ready_when: "steps.decide.metadata.outcome = \"rejected\"".into(),
                    terminal: Some(Terminal {
                        outcome: "scrapped".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    #[test]
    fn eager_materialization_creates_every_step() {
        // v2: every step materializes at Job open (no tier omission).
        // The trigger is Ready; the branches are Pending (their
        // predicates can't resolve until `decide` completes).
        let spec = fork_spec();
        let subject = Subject::new("account", "a-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);
        assert_eq!(steps.len(), 3, "all steps materialize eagerly");
        assert_eq!(steps[0].status, StepStatus::Ready, "trigger is Ready");
        assert_eq!(steps[1].status, StepStatus::Pending, "ship awaits decide");
        assert_eq!(steps[2].status, StepStatus::Pending, "scrap awaits decide");
        // blocked_by edges recovered from the predicates: both
        // branches depend on `decide`.
        assert_eq!(steps[1].blocked_by, vec![steps[0].id]);
        assert_eq!(steps[2].blocked_by, vec![steps[0].id]);
    }

    #[test]
    fn reevaluate_skips_branch_not_taken() {
        // After `decide` completes with outcome=approved, the
        // re-evaluator promotes `ship` to Ready and, because `scrap`'s
        // predicate is now provably false (its only ref `decide` is
        // terminal), marks `scrap` Skipped.
        let spec = fork_spec();
        let subject = Subject::new("account", "a-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut steps =
            materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);

        // Complete `decide` with the approved outcome.
        steps[0].status = StepStatus::Completed;
        steps[0].metadata = serde_json::json!({ "outcome": "approved" });

        let changed = reevaluate(&spec, &mut steps, &subject, &job_metadata);
        // Both branches changed status (ship → Ready, scrap → Skipped).
        assert!(changed.contains(&1), "ship should change: {changed:?}");
        assert!(changed.contains(&2), "scrap should change: {changed:?}");
        assert_eq!(
            steps[1].status,
            StepStatus::Ready,
            "approved branch is Ready"
        );
        assert_eq!(
            steps[2].status,
            StepStatus::Skipped,
            "branch not taken is Skipped"
        );
    }

    #[test]
    fn reevaluate_promotes_linear_dependent_to_ready() {
        // A simple chain: trigger → work(terminal). The dependent
        // sits Pending at open, then `reevaluate` promotes it to Ready
        // once the trigger completes. Returns the promoted index.
        let spec = JobKindSpec::platform_seed(
            "chain",
            "Chain",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "work".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        );
        let subject = Subject::new("account", "a-1");
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut steps =
            materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);
        assert_eq!(steps[1].status, StepStatus::Pending);
        // The dependent's blocked_by points at the trigger.
        assert_eq!(steps[1].blocked_by, vec![steps[0].id]);

        steps[0].status = StepStatus::Completed;
        let changed = reevaluate(&spec, &mut steps, &subject, &job_metadata);
        assert_eq!(changed, vec![1]);
        assert_eq!(steps[1].status, StepStatus::Ready);
    }

    // -----------------------------------------------------------
    // Trigger provenance — complete the firing trigger, skip the rest
    // -----------------------------------------------------------

    /// Mirrors `ingredient-restock`: two `auto-on-materialize` triggers
    /// (threshold + forecast) fanning into one downstream task gated on
    /// either via `steps.a.done OR steps.b.done`.
    fn two_trigger_spec() -> JobKindSpec {
        JobKindSpec::platform_seed(
            "restock-2trig",
            "Restock (two triggers)",
            "test",
            vec!["vendor".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    metadata_defaults: serde_json::json!({
                        "trigger_name": "inventory-reorder-threshold"
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "reorder-check".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    metadata_defaults: serde_json::json!({
                        "trigger_name": "demand-forecast-reorder"
                    }),
                    ..Default::default()
                },
                StepSpec {
                    title: "audit-stock".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done OR steps.reorder-check.done".into(),
                    ..Default::default()
                },
            ],
        )
    }

    #[test]
    fn materialize_completes_matching_trigger_skips_alternative() {
        // Provenance says the threshold trigger fired: it is born
        // Completed, the forecast alternative born Skipped, and the
        // downstream task promotes to Ready off the fired trigger — in
        // one materialization pass.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let reg = StepRegistry::v1();
        let job_metadata = serde_json::json!({ "trigger_name": "inventory-reorder-threshold" });
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &job_metadata,
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "the fired (threshold) trigger is Completed"
        );
        assert_eq!(
            steps[1].status,
            StepStatus::Skipped,
            "the alternative (forecast) trigger is the branch not taken"
        );
        assert_eq!(
            steps[2].status,
            StepStatus::Ready,
            "downstream task is Ready off the fired trigger's .done"
        );
    }

    #[test]
    fn materialize_with_no_provenance_fires_first_trigger() {
        // An operator-opened Job carries no `trigger_name`: the first
        // trigger fires (compat), the rest Skip — never all of them.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let reg = StepRegistry::v1();
        let job_metadata = serde_json::Value::Object(Default::default());
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &job_metadata,
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "first trigger fires"
        );
        assert_eq!(steps[1].status, StepStatus::Skipped, "the rest Skip");
        assert_eq!(steps[2].status, StepStatus::Ready);
    }

    #[test]
    fn materialize_single_trigger_is_born_completed() {
        // The common case: one trigger → it is born Completed at open,
        // never transient Ready work.
        let spec = JobKindSpec::platform_seed(
            "single-trig",
            "Single trigger",
            "test",
            vec!["account".into()],
            vec![
                StepSpec {
                    title: "trigger".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "work".into(),
                    kind: "task".into(),
                    ready_when: "steps.trigger.done".into(),
                    ..Default::default()
                },
            ],
        );
        let subject = Subject::new("account", "a-1");
        let reg = StepRegistry::v1();
        let job_metadata = serde_json::Value::Object(Default::default());
        let steps = materialize_steps_at(
            &spec,
            &subject,
            JobId::new(),
            &job_metadata,
            StepId::new,
            None,
            Some(&reg),
        );
        assert_eq!(
            steps[0].status,
            StepStatus::Completed,
            "sole trigger is born Completed"
        );
        assert_eq!(
            steps[1].status,
            StepStatus::Ready,
            "downstream promotes off it"
        );
    }

    #[test]
    fn materialize_without_registry_leaves_triggers_ready() {
        // The pure helper path (no registry) keeps the historical shape:
        // triggers materialize Ready. The live/sim create path always
        // passes the registry; this guards the no-arg `materialize_steps`
        // callers (tests, the dead trait default) from a behavior change.
        let spec = two_trigger_spec();
        let subject = Subject::new("vendor", "v-1");
        let job_metadata = serde_json::json!({ "trigger_name": "inventory-reorder-threshold" });
        let steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);
        assert_eq!(steps[0].status, StepStatus::Ready);
        assert_eq!(steps[1].status, StepStatus::Ready);
    }

    // -----------------------------------------------------------
    // bootstrap_reconcile — InMemoryJobKinds
    // -----------------------------------------------------------

    fn reconcile_spec(kind: &str, label: &str) -> JobKindSpec {
        JobKindSpec::platform_seed(kind, label, "platform", vec!["account".into()], Vec::new())
    }

    #[tokio::test]
    async fn bootstrap_reconcile_inserts_missing_kinds() {
        let registry = InMemoryJobKinds::new();
        let defaults = vec![reconcile_spec("job-kind-design", "Design a JobKind")];
        let stats = registry.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 0);
        let live = registry.get_active("job-kind-design").await.unwrap();
        assert_eq!(live.label, "Design a JobKind");
        assert_eq!(live.version, 1);
        assert_eq!(live.status, JobKindStatus::Active);
    }

    #[tokio::test]
    async fn bootstrap_reconcile_refreshes_drifted_bootstrap_rows() {
        let registry = InMemoryJobKinds::new();
        // Seed a stale bootstrap row.
        let stale = reconcile_spec("job-kind-design", "Old Label");
        registry
            .bootstrap_reconcile(&[stale])
            .await
            .expect("seed bootstrap");
        // Defaults now carry the corrected label.
        let updated = reconcile_spec("job-kind-design", "Design a JobKind");
        let stats = registry
            .bootstrap_reconcile(&[updated])
            .await
            .expect("refresh");
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 1);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 0);
        let live = registry.get_active("job-kind-design").await.unwrap();
        assert_eq!(live.label, "Design a JobKind", "drift should self-heal");
        // Refresh preserves version — never publishes a new one.
        assert_eq!(live.version, 1, "refresh must not bump version");
    }

    #[tokio::test]
    async fn bootstrap_reconcile_preserves_operator_edits() {
        let registry = InMemoryJobKinds::new();
        // Operator-owned row: inserted via `seed`, NOT via reconcile,
        // so it lands without bootstrap ownership tracking.
        let mut operator_spec = reconcile_spec("job-kind-design", "Operator Label");
        operator_spec.version = 1;
        operator_spec.status = JobKindStatus::Active;
        registry.seed(operator_spec).expect("seed operator row");

        let updated = reconcile_spec("job-kind-design", "Default Label");
        let stats = registry.bootstrap_reconcile(&[updated]).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 1);
        assert_eq!(stats.unchanged, 0);
        let live = registry.get_active("job-kind-design").await.unwrap();
        assert_eq!(
            live.label, "Operator Label",
            "operator edits must survive reconcile"
        );
    }

    #[tokio::test]
    async fn bootstrap_reconcile_no_op_when_already_matching() {
        let registry = InMemoryJobKinds::new();
        let spec = reconcile_spec("job-kind-design", "Design a JobKind");
        registry
            .bootstrap_reconcile(std::slice::from_ref(&spec))
            .await
            .expect("seed");
        let stats = registry.bootstrap_reconcile(&[spec]).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 1);
    }

    // -----------------------------------------------------------
    // publish_authored — InMemoryJobKinds
    // -----------------------------------------------------------

    #[tokio::test]
    async fn publish_authored_creates_v1_when_no_prior_rows() {
        let registry = InMemoryJobKinds::new();
        let spec = reconcile_spec("morning-brew", "Morning Brew");
        let job_id = JobId::new();

        let published = registry
            .publish_authored(spec, job_id)
            .await
            .expect("publish");

        assert_eq!(published.kind, "morning-brew");
        assert_eq!(published.version, 1);
        assert_eq!(published.status, JobKindStatus::Active);
        assert_eq!(
            published.authoring_job_id.expect("authoring set"),
            *job_id.inner().as_uuid(),
            "authoring_job_id must round-trip through publish"
        );

        let live = registry.get_active("morning-brew").await.unwrap();
        assert_eq!(live.version, 1);
    }

    #[tokio::test]
    async fn publish_authored_supersedes_prior_active_row() {
        let registry = InMemoryJobKinds::new();
        let job_a = JobId::new();
        let job_b = JobId::new();

        registry
            .publish_authored(reconcile_spec("morning-brew", "v1 label"), job_a)
            .await
            .expect("first publish");
        let updated = registry
            .publish_authored(reconcile_spec("morning-brew", "v2 label"), job_b)
            .await
            .expect("supersede");

        assert_eq!(updated.version, 2, "supersede must bump version");
        assert_eq!(updated.label, "v2 label");

        let active = registry.get_active("morning-brew").await.unwrap();
        assert_eq!(active.version, 2);
        assert_eq!(active.label, "v2 label");

        let v1 = registry.get_version("morning-brew", 1).await.unwrap();
        assert_eq!(v1.status, JobKindStatus::Retired);
    }

    // -----------------------------------------------------------
    // platform_kinds — the code-resident JobKinds reconciled into
    // the live registry on every boss-jobs-api start.
    // -----------------------------------------------------------

    #[test]
    fn platform_kinds_carries_job_kind_design_and_design_doc_review() {
        let kinds = platform_kinds();
        assert_eq!(kinds.len(), 2, "ships job-kind-design + design-doc-review");

        let design = kinds
            .iter()
            .find(|k| k.kind == "job-kind-design")
            .expect("job-kind-design present");
        assert_eq!(design.version, 1);
        assert_eq!(design.status, JobKindStatus::Active);
        assert_eq!(design.subject_kinds, vec!["custom".to_string()]);
        assert_eq!(design.owning_team, "platform");

        let review = kinds
            .iter()
            .find(|k| k.kind == "design-doc-review")
            .expect("design-doc-review present");
        assert_eq!(review.version, 1);
        assert_eq!(review.status, JobKindStatus::Active);
    }

    #[test]
    fn design_doc_review_materializes_doc_path_from_the_subject() {
        // The review-design step's doc_path must be stamped AT
        // materialization, atomically, from the Job's subject (the doc
        // path IS the subject id). The pre-fix seed defaulted it to ""
        // and left a second SPA PUT to fill it in — which lost a
        // read-overlay-write race against dispatcher assignment and
        // the sim workforce's completion, and terminal-metadata
        // immutability then sealed the empty value forever
        // ("Failed to load doc: step.metadata.doc_path is empty",
        // 2026-07-14).
        let kinds = platform_kinds();
        let review = kinds
            .iter()
            .find(|k| k.kind == "design-doc-review")
            .expect("design-doc-review present");

        let subject = Subject::new("custom", "docs/design/transactional-audit-log.md");
        let job_id = JobId::new();
        let job_metadata = serde_json::Value::Object(Default::default());
        let mut counter = 0u32;
        let steps = materialize_steps(review, &subject, job_id, &job_metadata, || {
            counter += 1;
            StepId::from_uuid(Uuid::from_u128(counter as u128))
        });

        // Spec order: open → review → reviewed (positional, matching
        // the sibling platform-kind step tests).
        let review_step = &steps[1];
        assert_eq!(review_step.kind, "review-design");
        assert_eq!(
            review_step
                .metadata
                .get("doc_path")
                .and_then(|v| v.as_str()),
            Some("docs/design/transactional-audit-log.md"),
            "doc_path must be the subject id at birth, not a later fill-in: {:?}",
            review_step.metadata
        );
    }

    #[test]
    fn platform_kinds_steps_run_design_to_publish_in_four_steps() {
        let kinds = platform_kinds();
        let design = kinds
            .iter()
            .find(|k| k.kind == "job-kind-design")
            .expect("job-kind-design present");
        // v2: flat steps, DAG implicit in ready_when. The lifecycle is
        // author → validate → approve(sign-off) → publish(terminal).
        let steps = &design.steps;
        assert_eq!(steps.len(), 4, "design lifecycle has 4 steps");
        assert_eq!(steps[0].kind, "task"); // author
        assert_eq!(steps[1].kind, "task"); // validate
        assert_eq!(steps[2].kind, "sign-off"); // approve
        assert_eq!(
            steps[2].sign_offs_required,
            vec!["job-kind-approver".to_string()]
        );
        assert_eq!(
            steps[2].authority_role.as_deref(),
            Some("job-kind-approver"),
            "approval is the operational-leader `job-kind-approver` authority, \
             not platform-admin alone"
        );
        assert_eq!(steps[3].kind, "job-kind-publish"); // publish
        assert!(steps[3].terminal.is_some(), "publish is the terminal step");

        // design-doc-review carries 3 steps: open → review → reviewed.
        let review = kinds
            .iter()
            .find(|k| k.kind == "design-doc-review")
            .expect("design-doc-review present");
        assert_eq!(review.steps.len(), 3, "review lifecycle has 3 steps");
    }

    #[test]
    fn platform_kinds_passes_validate_all() {
        use crate::job_kind_lint::validate_all;
        use crate::step_registry::StepRegistry;

        let kinds = platform_kinds();
        let registry = StepRegistry::v1();
        let errs = validate_all(&kinds, &registry);
        assert!(
            errs.is_empty(),
            "platform_kinds() must pass the same lint that gates every other kind: {errs:?}"
        );
    }

    #[test]
    fn platform_kinds_round_trip_through_json() {
        // The wire shape used by HTTP handlers + the audit_log
        // payload must round-trip cleanly. If anything in the
        // platform spec drifts from the deserializer's expectations
        // (a missing #[serde] attribute on a new JobKindSpec
        // field, say), this test is the early-warning line.
        let kinds = platform_kinds();
        for original in kinds {
            let wire = serde_json::to_value(&original).expect("serialize");
            let decoded: JobKindSpec = serde_json::from_value(wire).expect("deserialize");
            assert_eq!(decoded.kind, original.kind);
            assert_eq!(decoded.steps.len(), original.steps.len());
        }
    }

    #[tokio::test]
    async fn publish_authored_flips_row_out_of_bootstrap_ownership() {
        // Sequence: bootstrap reconcile → operator publish via Job →
        // next reconcile must preserve the operator-published row.
        let registry = InMemoryJobKinds::new();
        registry
            .bootstrap_reconcile(&[reconcile_spec("morning-brew", "Bootstrap Label")])
            .await
            .expect("seed bootstrap");

        registry
            .publish_authored(
                reconcile_spec("morning-brew", "Job-Authored Label"),
                JobId::new(),
            )
            .await
            .expect("publish via Job");

        let stats = registry
            .bootstrap_reconcile(&[reconcile_spec("morning-brew", "Default Label")])
            .await
            .expect("reconcile after publish");
        assert_eq!(
            stats.preserved, 1,
            "publish_authored must flip the row out of bootstrap ownership"
        );
        let live = registry.get_active("morning-brew").await.unwrap();
        assert_eq!(live.label, "Job-Authored Label");
    }
}
