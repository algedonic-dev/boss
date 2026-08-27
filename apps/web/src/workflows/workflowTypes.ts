// Mirror of boss_jobs::registry types (Workflow v2).
//
// v2 deletes the tier-based step graph. A Workflow is now a FLAT
// ordered list of steps; the DAG is implicit in each step's
// `ready_when` predicate. There is no `StepGraph`, `TierSpec`, or
// `StepEdge` anymore — the topological order emerges from the
// predicates referencing sibling step slugs (`steps.<title>.done`).

export type WorkflowStatus = 'draft' | 'active' | 'retired';

/// Terminal marker. When a step reaches Completed, the Job closes
/// with this outcome. Absent for non-terminal steps.
export type Terminal = {
  outcome: string;
};

export type StepSpec = {
  /// STABLE kebab-case slug, unique within the Workflow. Predicates
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
  /// Assurance the stamp must be produced with. Absent/null = the
  /// kind's floor (today: session). "presence" demands a passkey
  /// assertion bound to the step's shape hash — a Workflow may raise
  /// but never lower the floor (docs/design/presence.md Q1).
  assurance_required?: 'session' | 'presence' | null;
  metadata_defaults: Record<string, unknown>;
};

export type WorkflowSpec = {
  kind: string;
  version: number;
  status: WorkflowStatus;
  label: string;
  description: string | null;
  category: string;
  subject_kinds: ReadonlyArray<string>;
  steps: ReadonlyArray<StepSpec>;
  metadata_schema: Record<string, unknown>;
  /// Free-form Workflow-level metadata blob. Carries the `surfaces`
  /// hint (an array like `["hr"]` / `["qa"]`) declaring which
  /// operational pages this Workflow appears on — read via
  /// `workflowSurfaces`.
  metadata: Record<string, unknown>;
  entitlements: Record<string, unknown>;
  owning_team: string;
  authoring_job_id: string | null;
  created_at: string;
};

/// Safely read the `surfaces` hint off a Workflow's `metadata` blob.
/// Returns the declared operational-page slugs (e.g. `['hr']`,
/// `['qa']`) as a string[], or `[]` when the key is absent or
/// malformed. Operational pages (HR, QA) use this to discover which
/// Workflows belong to them instead of hardcoding tenant slugs.
export function workflowSurfaces(spec: {
  metadata?: Record<string, unknown>;
}): string[] {
  const surfaces = spec.metadata?.surfaces;
  if (!Array.isArray(surfaces)) return [];
  return surfaces.filter((s): s is string => typeof s === 'string');
}
