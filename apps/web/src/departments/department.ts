// A department's jobs view — in / working / out over the packets whose
// workflow declares the department.
//
// David, 2026-09-12: "each department will have a view of jobs flowing
// in, jobs getting worked within the department, and jobs flowing
// out." IT built its three first (Receiving Yard / Crew Board / Train
// Yard), each IT-shaped. This is the department-shaped instance: one
// read, three thirds, for every department the departments registry declares
// — the tab a surface-less department used to land on All jobs from
// (backlog cc76f755, 2026-09-18).
//
// WHICH PACKETS ARE THE DEPARTMENT'S is the server's question, not
// this file's: `GET /api/jobs?department=<code>` keeps the packets
// whose own `metadata.department` is `<code>` (a retro, a page audit,
// the items an audit files) and, for a packet naming none, those of
// the kinds whose active workflow row declares it (backlog 481d7939,
// `DepartmentFilter` in boss-jobs). Before that
// parameter existed the listing ignored it and answered the
// unfiltered count (1944 on prod), which is the reading this page
// must never make — so the loader keeps `total` and the page reports
// a truncated read rather than calling a page the world.
//
// WHICH THIRD A PACKET IS IN is derived here, from its steps, and the
// rule is deliberately the plainest one that is true:
//
//   out      — terminal (closed or cancelled). Its own read asks for
//              the last OUT_WINDOW_DAYS of them, so this third is
//              "what left recently", not the archive.
//   in       — live, and nothing has been done on it yet: no step has
//              started or finished. It stands at its first step.
//   working  — live, and something has: a step is active, or one has
//              completed and the next is waiting.
//
// A packet whose kind has no steps is inbound until it closes; a
// blocked packet is working (something happened, then it stopped).
// "First HUMAN step" — skipping past automated admission steps — is
// the sharper reading and needs the step's actor on the wire, which
// the listing does not carry; this rule is the honest first cut and
// says so.

import { fetchRemote, type Remote } from '../data/remote';
import { parseJob, type Job, type Step } from '../jobs/types';
import { lensNow, waitedText, type StepWaits } from '../jobs/queueAge';

/** The OUT third is the last thirty days of departures. */
export const OUT_WINDOW_DAYS = 30;
/** The live read's page: In and Working, the work a department holds.
 *  Sized so the busiest department's open work fits whole — IT's
 *  ~430 open backlog-items once its kinds are published (backlog
 *  a22311a1) — and a department past it is reported, not silently cut
 *  (a-limit-is-not-a-filter). */
export const LIVE_PAGE = 500;
/** The departures read's page: the Out third is "what left recently",
 *  a sample beside its total, never the archive — IT closes ~1,230
 *  packets a day, so no page could hold its thirty days. */
export const OUT_PAGE = 100;

export type Third = 'in' | 'working' | 'out';

export const THIRD_LABEL: Readonly<Record<Third, string>> = {
  in: 'In',
  working: 'Working',
  out: 'Out',
};

/** Which third a packet stands in, from its status and its steps. */
export function thirdOf(job: Pick<Job, 'status' | 'steps'>): Third {
  if (job.status === 'closed' || job.status === 'cancelled') return 'out';
  const steps: ReadonlyArray<Step> = job.steps ?? [];
  const moved = steps.some(
    (s) => s.status === 'active' || s.status === 'completed' || s.status === 'skipped',
  );
  return moved ? 'working' : 'in';
}

/** Where a live packet stands: the titles of the steps that can be
 *  taken now (ready or active), in the workflow's order. A terminal
 *  packet waits on nothing, and a live one with no open step answers
 *  '' rather than naming a step it is not at. Backlog 4d4dc204: a
 *  payout sat at `post` for 2.6 days and the finance page could not
 *  say so — a third says a packet is live; this says where. */
export function waitingAt(job: Pick<Job, 'status' | 'steps'>): string {
  return openSteps(job)
    .map((s) => s.title || s.kind)
    .join(' · ');
}

/** The steps a live packet stands at (ready or active), in workflow
 *  order — none for a terminal packet. `waitingAt` names them and
 *  `waitedFor` ages them, so the two columns line up step for step. */
function openSteps(job: Pick<Job, 'status' | 'steps'>): ReadonlyArray<Step> {
  if (job.status === 'closed' || job.status === 'cancelled') return [];
  return (job.steps ?? [])
    .filter((s) => s.status === 'ready' || s.status === 'active')
    .slice()
    .sort((a, b) => a.sort_order - b.sort_order);
}

/** Since when: how long each step `waitingAt` names has stood ready or
 *  active, joined the same way. The instant is the queue-age lens's
 *  (`jobs/queueAge.ts`), joined by step id, because the listing does
 *  not carry it — boss-jobs port.rs keeps it "A LENS, NOT A FIELD"
 *  (backlog 66a5d5be: the finance audit asked for "2.6 days at post"
 *  and the page could say only "at post"). A fallback stamp prints as
 *  `≥` — a floor; a step the lens has no row for says `unknown`; a
 *  failed lens says `unreadable` in the cell as well as on the page's
 *  own failure line. Never an age the lens did not give. */
export function waitedFor(
  job: Pick<Job, 'status' | 'steps'>,
  waits: Remote<StepWaits>,
  fallbackNowMs: number,
): string {
  const open = openSteps(job);
  if (open.length === 0) return '';
  if (waits.kind === 'loading') return '…';
  if (waits.kind === 'failed') return 'unreadable';
  const now = lensNow(waits.data, fallbackNowMs);
  return open
    .map((s) => {
      const w = waits.data.byStep.get(s.id);
      return w ? waitedText(w, now) : 'unknown';
    })
    .join(' · ');
}

export type Thirds = Readonly<{
  in: ReadonlyArray<Job>;
  working: ReadonlyArray<Job>;
  out: ReadonlyArray<Job>;
}>;

/** The three thirds, each in reading order: the inbound queue oldest
 *  first (what has waited longest leads), working newest first (the
 *  listing's own order), departures most recent first. */
export function thirds(jobs: ReadonlyArray<Job>): Thirds {
  const by = (t: Third): ReadonlyArray<Job> => jobs.filter((j) => thirdOf(j) === t);
  const asc = (a: Job, b: Job): number => a.opened_on.localeCompare(b.opened_on);
  const closedDesc = (a: Job, b: Job): number =>
    (b.closed_on ?? '').localeCompare(a.closed_on ?? '');
  return {
    in: by('in').slice().sort(asc),
    working: by('working'),
    out: by('out').slice().sort(closedDesc),
  };
}

export type JobsPage = Readonly<{ rows: ReadonlyArray<Job>; total: number }>;

/** The listing's envelope, kept whole: `total` is what the page
 *  compares itself against. Exec audit a1d62870 (2026-10-03): a
 *  malformed 200 used to become "No jobs", and missing lifecycle
 *  data became In. Require the table's identities and slim steps;
 *  missing step metadata is valid on a listing, missing steps is not. */
export function parseJobsPage(raw: unknown): JobsPage {
  const env = raw as { data?: unknown; total?: unknown } | null;
  if (!env || !Array.isArray(env.data) || typeof env.total !== 'number'
      || !Number.isSafeInteger(env.total) || env.total < env.data.length
      || (env.data.length === 0 && env.total !== 0)) {
    throw new Error('the department jobs read answered an invalid counted envelope');
  }
  const rows = env.data.map((rawRow) => {
    const row = parseJob('the department jobs read', rawRow);
    if (!row.id.trim() || typeof row.kind !== 'string' || !row.kind.trim() || typeof row.title !== 'string'
        || !['draft', 'open', 'closed', 'cancelled'].includes(row.status)
        || typeof row.priority !== 'string' || typeof row.opened_on !== 'string' || !row.opened_on.trim()
        || (row.closed_on !== null && typeof row.closed_on !== 'string')
        || typeof row.subject.id !== 'string' || !row.subject.id.trim()
        || typeof row.subject.subject_kind !== 'string' || !row.subject.subject_kind.trim()
        || !Array.isArray(row.steps)
        || row.steps.some((s) => !s || typeof s.id !== 'string' || !s.id.trim()
          || typeof s.title !== 'string' || typeof s.kind !== 'string'
          || !['pending', 'ready', 'active', 'completed', 'skipped'].includes(s.status)
          || !Number.isSafeInteger(s.sort_order))) {
      throw new Error('the department jobs read answered an invalid table row');
    }
    return row;
  });
  if (new Set(rows.map((row) => row.id)).size !== rows.length) {
    throw new Error('the department jobs read answered duplicate packet identities');
  }
  const total = env.total;
  return { rows, total };
}

// TWO READS, NOT ONE (backlog a22311a1). The view read live packets and
// the window's departures as ONE page of 200 (`closed_within`, live OR
// closed since), newest first. Once 61 platform kinds declared `it`,
// those kinds would open ~1,230 packets a day: the page would hold
// about four hours of closed chores and the 429 open IT backlog-items
// would fall off it, so In and Working read near-empty on the
// department with the most open work. The live packets and the
// departures are now two reads (`terminal=false` / `terminal=true`,
// the server's live-or-terminal filter), each bounded and each with
// its own `total`, so neither third can crowd out the other.

/** In and Working: every live (draft or open) packet in the department. */
export function liveJobsUrl(code: string): string {
  return `/api/jobs?department=${encodeURIComponent(code)}&terminal=false&limit=${LIVE_PAGE}`;
}

/** Out: the packets that closed or were cancelled in the window. */
export function departuresUrl(code: string): string {
  return `/api/jobs?department=${encodeURIComponent(code)}&terminal=true&closed_within=${OUT_WINDOW_DAYS}&limit=${OUT_PAGE}`;
}

export type DepartmentJobs = Readonly<{ live: JobsPage; out: JobsPage }>;

/** The three thirds from the two reads. Each third takes rows from its
 *  own read only — In and Working from the live page, Out from the
 *  departures — so a reply carrying both kinds of row cannot draw one
 *  packet twice. */
export function departmentThirds(d: DepartmentJobs): Thirds {
  const live = thirds(d.live.rows);
  return { in: live.in, working: live.working, out: thirds(d.out.rows).out };
}

export type Truncation = Readonly<{ read: 'live' | 'out'; shown: number; total: number }>;

/** The reads whose page is smaller than their total — each said on
 *  the page by name, never a page passed off as the department. */
export function truncatedReads(d: DepartmentJobs): ReadonlyArray<Truncation> {
  return (['live', 'out'] as const)
    .filter((read) => d[read].total > d[read].rows.length)
    .map((read) => ({ read, shown: d[read].rows.length, total: d[read].total }));
}

/** Both reads, concurrently. Either failing fails the view, with the
 *  failing read's own error: half a department drawn as the whole
 *  would be the false-empty this page exists to refuse. */
export async function loadDepartment(
  code: string,
): Promise<Exclude<Remote<DepartmentJobs>, { kind: 'loading' }>> {
  const [live, out] = await Promise.all([
    fetchRemote(liveJobsUrl(code), parseJobsPage),
    fetchRemote(departuresUrl(code), parseJobsPage),
  ]);
  if (live.kind === 'failed') return live;
  if (out.kind === 'failed') return out;
  return { kind: 'ready', data: { live: live.data, out: out.data } };
}
