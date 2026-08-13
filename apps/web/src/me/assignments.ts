// The My Day read surface: the assignments lens (queue-visibility Q1).
//
// One indexed call replaces the capped jobs?status=open scan — the
// server's WHERE is the queue definition, and this module only
// splits the rows into the two queues the page renders: mine
// (personal queue) and up-for-grabs (my role's group queue,
// unassigned). Rows where someone else is mid-flight on a
// role-matched step are visible context, not claimable work.

import { isSim, type PacketCardData } from '@boss/web-kit/ui/packet-card';

export type AssignmentStep = Readonly<{
  id: string;
  job_id: string;
  kind: string;
  spec_slug?: string | null;
  title: string;
  status: string;
  assignee_id?: string | null;
  metadata?: Record<string, unknown> | null;
}>;

export type AssignmentRow = Readonly<{
  job_id: string;
  job_title: string;
  due_on?: string | null;
  workflow: string;
  subject_kind: string;
  subject_id: string;
  priority: string;
  /** The Job's admission-fixed sim-vs-real flag, and its tags — the
   *  packet facts the card needs, carried on the row so this lens
   *  needs no second fetch. Optional so a response from a server that
   *  predates them still parses (they then read as a real packet, the
   *  same default the Job's own `serde(default)` takes). */
  simulated?: boolean;
  tags?: readonly string[];
  step: AssignmentStep;
}>;

export type MyDayQueues = Readonly<{
  mine: readonly AssignmentRow[];
  upForGrabs: readonly AssignmentRow[];
  inFlightElsewhere: readonly AssignmentRow[];
}>;

const PRIORITY_RANK: Record<string, number> = {
  emergency: 0,
  urgent: 1,
  standard: 2,
  scheduled: 3,
};

// Actionable before blocked, then priority, then due date — the
// same ordering the old client-side filter used, applied per row.
export function orderQueue(rows: readonly AssignmentRow[]): AssignmentRow[] {
  return [...rows].sort((a, b) => {
    const aa = a.step.status === 'ready' || a.step.status === 'active' ? 0 : 1;
    const ab = b.step.status === 'ready' || b.step.status === 'active' ? 0 : 1;
    if (aa !== ab) return aa - ab;
    const pa = PRIORITY_RANK[a.priority] ?? 3;
    const pb = PRIORITY_RANK[b.priority] ?? 3;
    if (pa !== pb) return pa - pb;
    if (a.due_on && b.due_on) return a.due_on.localeCompare(b.due_on);
    if (a.due_on) return -1;
    if (b.due_on) return 1;
    return 0;
  });
}

export function splitQueues(
  rows: readonly AssignmentRow[],
  uid: string,
): MyDayQueues {
  const mine = rows.filter(r => r.step.assignee_id === uid);
  const upForGrabs = rows.filter(r => !r.step.assignee_id);
  const inFlightElsewhere = rows.filter(
    r => r.step.assignee_id && r.step.assignee_id !== uid,
  );
  return {
    mine: orderQueue(mine),
    upForGrabs: orderQueue(upForGrabs),
    inFlightElsewhere: orderQueue(inFlightElsewhere),
  };
}

export async function fetchMyDay(
  uid: string,
  role: string,
): Promise<MyDayQueues | null> {
  const params = new URLSearchParams({ assignee_id: uid, roles: role });
  const resp = await fetch(`/api/jobs/assignments?${params}`);
  if (!resp.ok) return null;
  const body = (await resp.json()) as { data?: AssignmentRow[] };
  return splitQueues(body.data ?? [], uid);
}

// My Day rows render as packet cards — the same card the train yard
// uses (feedback d69033dd: one card grammar across the network). The
// lens maps its rows into the card's shape: the workflow is the
// protocol, the job title leads, and the actionable step rides the
// mono provenance line. Priority, due date, and a blocked marker
// travel as tag chips. Sim comes off the row's own packet facts
// through the shared predicate, so a simulated packet is as visibly
// simulated in a personal queue as it is in the yard. The job's tags
// feed that predicate but stay off the chips: in this lens the chips
// are queue state (blocked / priority / due), not packet labels.
export function assignmentPacket(row: AssignmentRow): PacketCardData {
  const actionable = row.step.status === 'ready' || row.step.status === 'active';
  return {
    id: row.job_id,
    kind: row.workflow,
    branch: row.step.title,
    title: row.job_title,
    tags: [
      ...(actionable ? [] : ['blocked']),
      ...(row.priority !== 'standard' ? [row.priority] : []),
      ...(row.due_on ? [`due ${row.due_on}`] : []),
    ],
    sim: isSim({ simulated: row.simulated, tags: row.tags }),
    skipReason: null,
  };
}

export type ClaimResult =
  | { kind: 'claimed' }
  | { kind: 'conflict'; holder: string | null; status: string }
  | { kind: 'error'; message: string };

// The claim hop: the CAS endpoint decides; a 409 names the holder so
// the queue can say "taken by X" instead of failing blankly.
export async function claimStep(
  jobId: string,
  stepId: string,
): Promise<ClaimResult> {
  const resp = await fetch(
    `/api/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(stepId)}/claim`,
    { method: 'POST' },
  );
  if (resp.ok) return { kind: 'claimed' };
  if (resp.status === 409) {
    const body = (await resp.json().catch(() => ({}))) as {
      holder?: string | null;
      status?: string;
    };
    return {
      kind: 'conflict',
      holder: body.holder ?? null,
      status: body.status ?? 'unknown',
    };
  }
  return { kind: 'error', message: `HTTP ${resp.status}` };
}
