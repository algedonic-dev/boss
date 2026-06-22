// Client for authoring a JobKind *through* a `job-kind-design` Job
// (decision D6). The working spec lives in the design Job's publish-step
// `metadata.job_kind_spec`; the registry write + `jobs.kind.published`
// audit fact happen exactly once, when the terminal `job-kind-publish`
// step completes. No `job_kinds` draft rows; the only persistence while
// editing is `STEP_UPDATED` events on the design Job itself.
//
// These are thin, typed fetch wrappers over the existing job/step API —
// the same endpoints any Job uses. The only pure piece (`initialSpec`)
// is unit-tested; the rest is I/O verified end-to-end against the stack.

import type { JobKindSpec, StepSpec } from './jobKindTypes';
import type { Job, Step } from '../jobs/types';

export const DESIGN_KIND = 'job-kind-design';
export const PUBLISH_STEP_KIND = 'job-kind-publish';
/// The authority the `approve` sign-off step requires. Granted (via
/// policy) to the C-suite/COO/dept-heads who own the job-kinds surface,
/// plus platform-admin — see boss-jobs job_kind_design_spec + the tenant
/// policy seeds.
export const APPROVE_ROLE = 'job-kind-approver';

/// A complete, viable seed spec for a brand-new kind: a single trigger
/// step (`ready_when = "true"`) that is also terminal — the minimal
/// publishable JobKind (open and close). `created_at`/`version`/`status`
/// are placeholders; `publish_authored` stamps the real values when the
/// publish step fires.
export function initialSpec(
  slug: string,
  label: string,
  category: string,
  subjectKinds: ReadonlyArray<string>,
  description?: string,
): JobKindSpec {
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
export function readSpec(publishStep: Step | undefined): JobKindSpec | null {
  const v = publishStep?.metadata?.['job_kind_spec'];
  return v != null ? (v as JobKindSpec) : null;
}

export function findStep(
  steps: ReadonlyArray<Step>,
  kind: string,
): Step | undefined {
  return steps.find((s) => s.kind === kind);
}

/// Create the `job-kind-design` Job. Its subject is `{custom, <slug>}` —
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
  seedSpec: JobKindSpec,
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

/// Persist the working spec onto the publish step's metadata. The step
/// PATCH replaces the `metadata` field wholesale, so we send the COMPLETE
/// object — preserving any existing keys and (optionally) stamping
/// `previous_kind_version` for a new version of an existing kind.
export async function persistSpec(
  jobId: string,
  publishStep: Step,
  spec: JobKindSpec,
  previousVersion?: number,
): Promise<void> {
  const metadata: Record<string, unknown> = {
    ...publishStep.metadata,
    job_kind_spec: spec,
  };
  if (previousVersion != null) metadata['previous_kind_version'] = previousVersion;
  const r = await fetch(
    `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(publishStep.id)}`,
    {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ metadata }),
    },
  );
  if (!r.ok) {
    throw new Error(`persist spec: HTTP ${r.status}: ${await r.text()}`);
  }
}

export async function completeStep(jobId: string, stepId: string): Promise<void> {
  const r = await fetch(
    `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(stepId)}`,
    {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ status: 'completed' }),
    },
  );
  if (!r.ok) {
    throw new Error(`complete step: HTTP ${r.status}: ${await r.text()}`);
  }
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
