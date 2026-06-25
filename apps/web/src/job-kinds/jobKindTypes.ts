// Mirror of boss_jobs::registry types (JobKind v2).
//
// v2 deletes the tier-based step graph. A JobKind is now a FLAT
// ordered list of steps; the DAG is implicit in each step's
// `ready_when` predicate. There is no `StepGraph`, `TierSpec`, or
// `StepEdge` anymore — the topological order emerges from the
// predicates referencing sibling step slugs (`steps.<title>.done`).

export type JobKindStatus = 'draft' | 'active' | 'retired';

/// Terminal marker. When a step reaches Completed, the Job closes
/// with this outcome. Absent for non-terminal steps.
export type Terminal = {
  outcome: string;
};

export type StepSpec = {
  /// STABLE kebab-case slug, unique within the JobKind. Predicates
  /// reference it as `steps.<title>.done` /
  /// `steps.<title>.metadata.<field>`. This is NOT human display —
  /// `title_template` is the display string.
  title: string;
  /// StepType slug (from /api/jobs/step-types).
  kind: string;
  /// `ready_when` predicate. `"true"` marks a trigger that fires at
  /// Job open. See the grammar in StepDagEditor.svelte.
  ready_when: string;
  /// When set, reaching Completed on this step closes the Job with the
  /// given outcome. Non-terminal steps OMIT this field: the API uses
  /// serde `skip_serializing_if`, so it reads back as `undefined`, not
  /// `null`. The editor writes `null` when you untick "terminal". Treat
  /// absent / null identically — always check it truthily, never `!== null`.
  terminal?: Terminal | null;
  /// Human display template; `{subject.id}` etc. expand at runtime.
  /// Blank → humanized `title`.
  title_template: string;
  sign_offs_required?: string[];
  authority_role: string | null;
  metadata_defaults: Record<string, unknown>;
};

export type JobKindSpec = {
  kind: string;
  version: number;
  status: JobKindStatus;
  label: string;
  description: string | null;
  category: string;
  subject_kinds: ReadonlyArray<string>;
  steps: ReadonlyArray<StepSpec>;
  metadata_schema: Record<string, unknown>;
  /// Free-form JobKind-level metadata blob. Carries the `surfaces`
  /// hint (an array like `["hr"]` / `["qa"]`) declaring which
  /// operational pages this JobKind appears on — read via
  /// `jobKindSurfaces`.
  metadata: Record<string, unknown>;
  entitlements: Record<string, unknown>;
  owning_team: string;
  authoring_job_id: string | null;
  created_at: string;
};

/// Safely read the `surfaces` hint off a JobKind's `metadata` blob.
/// Returns the declared operational-page slugs (e.g. `['hr']`,
/// `['qa']`) as a string[], or `[]` when the key is absent or
/// malformed. Operational pages (HR, QA) use this to discover which
/// JobKinds belong to them instead of hardcoding tenant slugs.
export function jobKindSurfaces(spec: {
  metadata?: Record<string, unknown>;
}): string[] {
  const surfaces = spec.metadata?.surfaces;
  if (!Array.isArray(surfaces)) return [];
  return surfaces.filter((s): s is string => typeof s === 'string');
}
