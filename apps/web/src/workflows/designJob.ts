// Client for authoring a Workflow *through* a `workflow-design` Job
// (decision D6). The working spec lives in the design Job's publish-step
// `metadata.workflow_spec`; the registry write + `jobs.kind.published`
// audit fact happen exactly once, when the terminal `workflow-publish`
// step completes. No `workflows` draft rows; the only persistence while
// editing is `STEP_UPDATED` events on the design Job itself.
//
// These are thin, typed fetch wrappers over the existing job/step API —
// the same endpoints any Job uses. The only pure piece (`initialSpec`)
// is unit-tested; the rest is I/O verified end-to-end against the stack.

import type { WorkflowSpec, StepSpec } from './workflowTypes';
import type { Job, Step } from '../jobs/types';
import { putStep, saveStep } from '../steps/stepWrite';

export const DESIGN_KIND = 'workflow-design';
export const PUBLISH_STEP_KIND = 'workflow-publish';
/// The authority the `approve` sign-off step requires. Granted (via
/// policy) to the C-suite/COO/dept-heads who own the workflows surface,
/// plus platform-admin — see boss-jobs workflow_design_spec + the tenant
/// policy seeds.
export const APPROVE_ROLE = 'workflow-approver';

/// A complete, viable seed spec for a brand-new kind: a single trigger
/// step (`ready_when = "true"`) that is also terminal — the minimal
/// publishable Workflow (open and close). `created_at`/`version`/`status`
/// are placeholders; `publish_authored` stamps the real values when the
/// publish step fires.
export function initialSpec(
  slug: string,
  label: string,
  category: string,
  subjectKinds: ReadonlyArray<string>,
  description?: string,
): WorkflowSpec {
  const firstStep: StepSpec = {
    title: 'first-step',
    kind: 'generic',
    ready_when: 'true',
    terminal: { outcome: 'completed' },
    title_template: '',
    sign_offs_required: [],
    authority_role: null,
    metadata_defaults: {},
  };
  return {
    kind: slug,
    version: 1,
    status: 'draft',
    label,
    description: description ?? null,
    category,
    subject_kinds: [...subjectKinds],
    steps: [firstStep],
    metadata_schema: {},
    metadata: {},
    entitlements: {},
    owning_team: 'authoring',
    authoring_job_id: null,
    // Placeholder — the publish step stamps the real timestamp.
    created_at: '1970-01-01T00:00:00.000Z',
  };
}

/// Read the working spec out of the publish step's metadata, or null if
/// it hasn't been seeded yet.
export function readSpec(publishStep: Step | undefined): WorkflowSpec | null {
  const v = publishStep?.metadata?.['workflow_spec'];
  return v != null ? (v as WorkflowSpec) : null;
}

export function findStep(
  steps: ReadonlyArray<Step>,
  kind: string,
): Step | undefined {
  return steps.find((s) => s.kind === kind);
}

/// Create the `workflow-design` Job. Its subject is `{custom, <slug>}` —
/// the slug is the Job's immutable subject id (D1). Steps materialize on
/// create. Returns the new Job id.
export async function createDesignJob(
  slug: string,
  ownerId: string,
  openedOn: string,
  title: string,
): Promise<string> {
  const body = {
    kind: DESIGN_KIND,
    subject: { subject_kind: 'custom', id: slug },
    title,
    owner_id: ownerId,
    status: 'open',
    priority: 'standard',
    opened_on: openedOn,
    metadata: {},
    tags: [],
  };
  const r = await fetch('/api/jobs', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    throw new Error(`create design job: HTTP ${r.status}: ${await r.text()}`);
  }
  const created = (await r.json()) as { id: string };
  return created.id;
}

export async function loadDesignJob(jobId: string): Promise<Job> {
  const r = await fetch(`/api/jobs/${encodeURIComponent(jobId)}`);
  if (!r.ok) throw new Error(`load design job: HTTP ${r.status}`);
  return (await r.json()) as Job;
}

/// Create a design Job for `seedSpec.kind` and seed its publish step with
/// `seedSpec`, returning the new Job id. The single entry point both
/// "new kind" and "edit/new-version" route through. `previousVersion`
/// stamps the publish step when branching from an existing active row.
export async function startDesignJob(
  seedSpec: WorkflowSpec,
  ownerId: string,
  openedOn: string,
  opts?: { title?: string; previousVersion?: number },
): Promise<string> {
  const slug = seedSpec.kind;
  const jobId = await createDesignJob(
    slug,
    ownerId,
    openedOn,
    opts?.title ?? `Design ${slug}`,
  );
  const job = await loadDesignJob(jobId);
  const publish = findStep(job.steps ?? [], PUBLISH_STEP_KIND);
  if (publish) {
    await persistSpec(jobId, publish, seedSpec, opts?.previousVersion);
  }
  return jobId;
}

/// Persist the working spec onto the publish step's metadata, through
/// the step merge door (backlog e39a9d2a, Stage 2): only `workflow_spec`
/// and, for a new version of an existing kind, `previous_kind_version`.
/// It used to PUT a spread of the step's metadata as the page last read
/// it, so a key written since — a claim, a concurrent autosave's — went
/// back as it was, and under the omission rule a new one was refused.
export async function persistSpec(
  jobId: string,
  publishStep: Step,
  spec: WorkflowSpec,
  previousVersion?: number,
): Promise<void> {
  const metadata: Record<string, unknown> = {
    workflow_spec: spec,
    ...(previousVersion != null ? { previous_kind_version: previousVersion } : {}),
  };
  const r = await saveStep(jobId, publishStep.id, { metadata });
  if (r.kind === 'failed') throw new Error(`persist spec: ${r.error}`);
}

export async function completeStep(jobId: string, stepId: string): Promise<void> {
  const r = await putStep(jobId, stepId, { status: 'completed' });
  if (r.kind === 'failed') throw new Error(`complete step: ${r.error}`);
}

/// Stamp a required sign-off role on a sign-off step. Must precede
/// completing the step; the server 409s a completion whose required
/// roles haven't all signed the current shape.
export async function signOff(
  jobId: string,
  stepId: string,
  role: string,
): Promise<void> {
  const r = await fetch(
    `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(stepId)}/sign-offs`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ role }),
    },
  );
  if (!r.ok) {
    throw new Error(`sign-off: HTTP ${r.status}: ${await r.text()}`);
  }
}
