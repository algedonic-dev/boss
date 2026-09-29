// Shared write path for the platform step surfaces (packet cc9d7fc6).
//
// The class this exists to kill: surfaces that `await fetch(...)` a
// PUT and ignore the response. A Complete click that 400s did nothing
// visible — the operator believed the step closed. Every step-surface
// write now flows through here and comes back as a discriminated
// result the surface must branch on: `ok` continues, `failed` renders
// inline and leaves state untouched.
//
// The doors themselves — writeStep, saveStep, putStep and the merge
// door — live in `@boss/web-kit/step-doors`, the one file in the web
// that builds a step's URL (backlog e39a9d2a, Stage 2 car 2), so the
// kit's own chrome writes a step the same way. They are re-exported
// here so a surface imports every step write from one place.

import {
  type StepWriteResult,
  type WriteOpts,
  mergeStepMetadata,
  putStep,
  writeStep,
} from '@boss/web-kit/step-doors';
import { RELEASE, releaseMetadata, releaseUnconfirmed } from './holder';

export {
  type StepPutBody,
  type StepWriteResult,
  type WriteOpts,
  WRITE_RETRY,
  describeWriteFailure,
  mergeStepMetadata,
  putStep,
  saveStep,
  writeStep,
} from '@boss/web-kit/step-doors';

/// Start a step: the claim door, `POST …/steps/{id}/claim` — the one
/// door a step becomes Active through (design 611fbffd, clause b of
/// backlog 6ef4a36b). Every surface's Start was its own step PUT moving
/// the status to active, so the record could not tell "X took this work" from
/// "someone assigned X and started the clock", and nothing enforced
/// "release, then claim". The door is a compare-and-set on a READY step
/// (a loser gets 409 naming the holder), reserves the holder's calendar
/// time exactly as the PUT did, and records who started it for whom.
///
/// `claimedFor` names the holder when it is not the caller — the step's
/// nominee, or the picker's choice ([`claimedFor`](./holder.ts)). The
/// server admits that only for the step's declared executor or a holder
/// of `step-assign`, and its refusal is the surface's error.
///
/// Resent through a deploy roll like a PUT: the claim is idempotent for
/// its holder, so a claim that landed before the blip answers the step
/// it already took rather than taking it twice.
export function startStep(
  jobId: string,
  stepId: string,
  claimedFor?: string | null,
  opts?: WriteOpts,
): Promise<StepWriteResult> {
  const holder = (claimedFor ?? '').trim();
  const query = holder ? `?claimed_for=${encodeURIComponent(holder)}` : '';
  return writeStep(
    `/api/jobs/${jobId}/steps/${stepId}/claim${query}`,
    { method: 'POST' },
    { ...opts, idempotent: true },
  );
}

/// A release's outcome. `partial` is the third answer a two-write act
/// owes: the merge landed — the reason recorded, the run edge cleared —
/// and then the status write was refused, or the read-back does not
/// bear the release out. It is neither success nor a clean failure,
/// and a surface that rendered it as either would lie about the step.
export type ReleaseResult =
  | { kind: 'ok' }
  | { kind: 'failed'; error: string }
  | { kind: 'partial'; error: string };

/// Release an active step — the three acts `boss step release` makes,
/// so the page is a third way to hand a step back rather than a
/// different one (backlog 6ef4a36b: the surfaces said a held step
/// changes hands "by release", and offered no release):
///
/// 1. the merge door: the run edge cleared and the `released` stamp
///    `{why, by, at, from_run}` recorded, in one write
///    ([`releaseMetadata`]);
/// 2. the PUT: [`RELEASE`] — `ready`, nobody's;
/// 3. the read-back, because a 2xx is a claim and the step as it now
///    reads is the fact ([`releaseUnconfirmed`]).
///
/// A blank reason writes nothing, as the CLI refuses a blank `--why`: the
/// reason is the whole artifact a release leaves (the review of car
/// 675f1858, #2). The next holder takes the step through the claim.
export async function releaseStep(
  jobId: string,
  step: Readonly<{ id: string; metadata: Readonly<Record<string, unknown>> }>,
  why: string,
  by: string | null,
  opts?: WriteOpts,
): Promise<ReleaseResult> {
  if (!why.trim()) {
    return {
      kind: 'failed',
      error: 'a release needs a reason — it is the whole record a release leaves',
    };
  }
  const merged = await mergeStepMetadata(
    jobId,
    step.id,
    releaseMetadata(step.metadata, why, by, new Date().toISOString()),
    opts,
  );
  if (merged.kind === 'failed') return merged;

  const put = await putStep(jobId, step.id, RELEASE, undefined, opts);
  const readBack = await readStepBack(jobId, step.id, opts);
  const standing =
    typeof readBack === 'string' ? readBack : releaseUnconfirmed(readBack, why);
  if (put.kind === 'failed') {
    return {
      kind: 'partial',
      error:
        `PARTIAL RELEASE — the reason is recorded and the run edge cleared, but the ` +
        `status write was refused (${put.error}); ${standing ?? 'the step nonetheless reads back released'}. ` +
        'Reload before acting on this step.',
    };
  }
  if (standing !== null) {
    return {
      kind: 'partial',
      error: `PARTIAL RELEASE — both writes were accepted, but ${standing}. Reload before acting on this step.`,
    };
  }
  return { kind: 'ok' };
}

/// The step as the server now holds it, or a line saying why it could
/// not be read.
async function readStepBack(
  jobId: string,
  stepId: string,
  opts?: WriteOpts,
): Promise<
  | Readonly<{ status: string; assignee_id: string | null; metadata: Record<string, unknown> }>
  | string
> {
  const res = await writeStep(`/api/jobs/${jobId}/steps`, { method: 'GET' }, opts);
  if (res.kind === 'failed') return `the step could not be read back (${res.error})`;
  try {
    const rows = (await res.response.json()) as ReadonlyArray<{
      id: string;
      status: string;
      assignee_id: string | null;
      metadata?: Record<string, unknown> | null;
    }>;
    const row = rows.find((r) => r.id === stepId);
    if (!row) return 'the step is missing from the read-back';
    return {
      status: row.status,
      assignee_id: row.assignee_id ?? null,
      metadata: row.metadata ?? {},
    };
  } catch {
    return 'the read-back was not the step list';
  }
}
