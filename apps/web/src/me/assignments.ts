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
  /** The step kind's completion contract, from the StepType registry.
   *  Optional so a response from a server that predates it still
   *  parses — {@link needsAPerson} reads absent as human, the safe
   *  direction. */
  completion?: Completion | null;
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

/// How a step reaches `completed` — the server sends the step kind's
/// `StepType::completion` on every row. `null` when this deployment's
/// registry does not know the kind.
export type Completion =
  | 'human'
  | 'agent'
  | 'child-job'
  | 'external'
  | 'auto-on-materialize';

/// The step kinds whose completion IS a decision — a verdict someone
/// renders, not work someone performs. Decided by David (d598681f,
/// accepted 2026-08-19): My Day partitions "yours to DECIDE" from
/// "yours because you own it", on exactly the kind + completion facts
/// the row already carries. A kind roster is a deliberate,
/// presentation-only trade: it can lag a new decision kind (which then
/// lands in the owned list — visible, never lost), and the honest
/// upgrade is a `decision_shaped` flag on the StepType registry when a
/// second consumer wants this split.
export const VERDICT_KINDS: ReadonlySet<string> = new Set([
  'sign-off',
  'answer-question',
  'correction-verdict',
  'review-design',
]);

export function isVerdict(row: AssignmentRow): boolean {
  return (
    VERDICT_KINDS.has(row.step.kind) &&
    (row.step.completion ?? 'human') === 'human'
  );
}

export type MyDayQueues = Readonly<{
  /** Assigned to you AND verdict-shaped: sign-offs, reviews,
   *  corrections — the "what actually needs ME" list (d598681f). */
  verdicts: readonly AssignmentRow[];
  mine: readonly AssignmentRow[];
  /** Unassigned, role-matched, and a PERSON has to do it. */
  upForGrabs: readonly AssignmentRow[];
  /** Unassigned, role-matched, and the protocol says something other
   *  than a person completes it. See {@link needsAPerson}. */
  notMineToDo: readonly AssignmentRow[];
  inFlightElsewhere: readonly AssignmentRow[];
}>;

/// Does clearing this step require a human?
///
/// David, 2026-08-16: *"it probably makes sense to have a special
/// separation between jobs that are in a queue with a human-only
/// policy with jobs that agents are also eligible for as a practical
/// consideration"* — and the reason it is worth the column: *"We
/// intentionally do not want many protocols where policy requires a
/// human because that is slow."*
///
/// The predicate is `completion === 'human'`, not a list of kinds.
/// Five of the registry's kinds are `agent`, one is `child-job`, one
/// is `auto-on-materialize`, and the rest default to `human` — but
/// that census is registry data and will move, so the frontend must
/// not hold a copy of it (CLAUDE.md §9a).
///
/// UNKNOWN COUNTS AS HUMAN. A missing contract puts the packet in
/// front of somebody rather than filing it under "an agent will get
/// to it", and being wrong in that direction costs a glance instead
/// of a stalled packet.
export function needsAPerson(row: AssignmentRow): boolean {
  const c = row.step.completion;
  return c === undefined || c === null || c === 'human';
}

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
  const assigned = rows.filter(r => r.step.assignee_id === uid);
  const verdicts = assigned.filter(isVerdict);
  const mine = assigned.filter(r => !isVerdict(r));
  const unclaimed = rows.filter(r => !r.step.assignee_id);
  const inFlightElsewhere = rows.filter(
    r => r.step.assignee_id && r.step.assignee_id !== uid,
  );
  return {
    verdicts: orderQueue(verdicts),
    mine: orderQueue(mine),
    upForGrabs: orderQueue(unclaimed.filter(needsAPerson)),
    // An `agent`-completion step sitting unclaimed in a person's queue
    // is not work waiting for them — the dispatcher executes those on
    // `step.ready` and the human workforce never pulls them. So a row
    // here means the automation did not run, which is worth seeing and
    // is NOT worth burying in the same list as real work.
    notMineToDo: orderQueue(unclaimed.filter(r => !needsAPerson(r))),
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

// ---------------------------------------------------------------------
// Protocol filtering (David, 2026-08-14: "protocol-based My Queue
// filtering ... will make it much easier to see the specific jobs I
// want").
//
// Purely a lens over rows already fetched — `workflow` rides on every
// AssignmentRow, so this needs no second call and no server change.
// The counts come from the SAME rows the chips filter, so a chip can
// never advertise a number the list then contradicts.
// ---------------------------------------------------------------------

/** Every protocol present across the queues, with how many rows each
 *  holds, most-work-first then alphabetical. Counting all three queues
 *  together is deliberate: the question a chip answers is "how much
 *  approval work is in front of me", not "how much of it is already
 *  mine". */
export function protocolCounts(
  queues: MyDayQueues | null,
): ReadonlyArray<{ workflow: string; count: number }> {
  if (!queues) return [];
  // Every queue the page can render, `notMineToDo` included — a chip
  // that undercounts sends the reader to a protocol filter that then
  // shows rows the chip said were not there.
  const all = [
    ...queues.mine,
    ...queues.upForGrabs,
    ...queues.notMineToDo,
    ...queues.inFlightElsewhere,
  ];
  const tally = new Map<string, number>();
  for (const r of all) {
    if (!r.workflow) continue;
    tally.set(r.workflow, (tally.get(r.workflow) ?? 0) + 1);
  }
  return [...tally.entries()]
    .map(([workflow, count]) => ({ workflow, count }))
    .sort((a, b) => (b.count - a.count) || a.workflow.localeCompare(b.workflow));
}

/** `null` means no filter — every row. An unknown protocol yields an
 *  empty list rather than everything, so a stale chip selection reads
 *  as "nothing here" instead of silently widening the queue. */
export function filterByProtocol(
  rows: readonly AssignmentRow[],
  workflow: string | null,
): readonly AssignmentRow[] {
  if (workflow === null) return rows;
  return rows.filter((r) => r.workflow === workflow);
}
