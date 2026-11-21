// A view of human-required IT work, not the viewer's assignment queue
// (David, cef1f661). Only the server reads human_only's two spellings.
import { fetchRemote, type Remote } from '../../data/remote';
import { ageDays } from '../receiving/receiving';

/** A count as drawn: a value, a zero, a floor (a count cut at its limit,
 *  `N+`, backlog 97b5cf45), or unread with why — never a zero where
 *  nothing was read. It lived in the HUD frame until R3 (backlog
 *  23a4b4d7) retired every other row of it; the needs-you board is the
 *  one /it keeps. */
export type Figure =
  | Readonly<{ kind: 'value'; text: string; troubled: boolean }>
  | Readonly<{ kind: 'zero' }>
  | Readonly<{ kind: 'floor'; text: string; troubled: boolean; why: string }>
  | Readonly<{ kind: 'unread'; why: string }>;

/** A board: its lines, capped, with a count of the rest; or none, in
 *  words; or unread, with why. */
export type Board<L, X = Readonly<Record<never, never>>> =
  | (Readonly<{ kind: 'rows'; count: Figure; lines: ReadonlyArray<L>; more: number }> & X)
  | (Readonly<{ kind: 'empty'; count: Figure; words: string }> & X)
  | Readonly<{ kind: 'unread'; why: string }>;

/** How many lines the board shows before it counts the rest. */
export const SHOWN = 5;

/** A line of the view. `act` and `reserved_for` ride a reserved release
 *  the door would grant its actor now: the car's review step, derived on
 *  read by the server (design 58c72d95 D4). */
type HumanStep = Readonly<{
  id: string; kind: string; title: string; status: string; human_required: boolean;
  act?: string; reserved_for?: string;
}>;
type HumanJob = Readonly<{
  id: string; kind: string; title: string; status: string; priority: string;
  opened_on: string | null; steps: ReadonlyArray<HumanStep>;
}>;
export type NeedsYou = Readonly<{
  rows: ReadonlyArray<HumanJob>; total: number; limit: number; offset: number; cut: string | null;
}>;
export type NeedLine = Readonly<{
  id: string; href: string; step: string; protocol: string; title: string;
  stepTitle: string; priority: string | null; age: string;
}>;

export const NEEDS_YOU_LIMIT = 500;
// Four thin pages per poll, never an unbounded full-packet fanout. A
// larger population remains an explicit floor, not an exact count.
const PAGE_BUDGET = 4;
export const NEEDS_YOU_URL = `/api/jobs?status=open&department=it&human_required_view=true&limit=${NEEDS_YOU_LIMIT}`;

function object(raw: unknown): Record<string, unknown> {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) throw new Error('human-required view: expected an object');
  return raw as Record<string, unknown>;
}
function text(raw: unknown): string {
  if (typeof raw !== 'string') throw new Error('human-required view: missing text');
  return raw;
}
function integer(raw: unknown): number {
  if (typeof raw !== 'number' || !Number.isSafeInteger(raw) || raw < 0) throw new Error('human-required view: missing page count');
  return raw;
}

export function parseNeedsYou(raw: unknown): NeedsYou {
  const page = object(raw);
  if (page.human_required_view !== 1) throw new Error('this server does not answer the canonical human-required view');
  if (!Array.isArray(page.data)) throw new Error('human-required view: missing jobs');
  const total = integer(page.total);
  const limit = integer(page.limit);
  const offset = integer(page.offset);
  if (limit < 1 || limit > NEEDS_YOU_LIMIT || page.data.length > limit) throw new Error('human-required view: invalid page bound');
  const rows = page.data.map((rawJob): HumanJob => {
    const job = object(rawJob);
    if (!Array.isArray(job.steps)) throw new Error('human-required view: missing steps');
    return {
      id: text(job.id), kind: text(job.kind), title: text(job.title), status: text(job.status),
      priority: text(job.priority), opened_on: job.opened_on === null ? null : text(job.opened_on),
      steps: job.steps.map((rawStep): HumanStep => {
        const step = object(rawStep);
        if (typeof step.human_required !== 'boolean') throw new Error('human-required view: step lacks canonical declaration');
        return {
          id: text(step.id), kind: text(step.kind), title: text(step.title), status: text(step.status), human_required: step.human_required,
          ...(step.act === undefined ? {} : { act: text(step.act) }),
          ...(step.reserved_for === undefined ? {} : { reserved_for: text(step.reserved_for) }),
        };
      }),
    };
  });
  if (offset + rows.length > total) throw new Error('human-required view: page exceeds total');
  const cut = page.truncated === true ? `${rows.length} of at least ${total} jobs read — the answer was cut at its limit` : null;
  return { rows, total, limit, offset, cut };
}

type Answer = Exclude<Remote<NeedsYou>, { kind: 'loading' }>;
/** `base` is the view's first page without its offset: IT's, or a
 *  station page's over the protocols it holds (station-answer.ts
 *  `needsYouUrl`, backlog 950ad972) — paged and refused the same way. */
export async function fetchNeedsYou(
  read: (url: string) => Promise<Answer> = (url) => fetchRemote(url, parseNeedsYou),
  base: string = NEEDS_YOU_URL,
): Promise<Answer> {
  const rows: HumanJob[] = [];
  const ids = new Set<string>();
  let total: number | null = null;
  for (let page = 0; page < PAGE_BUDGET; page += 1) {
    const answer = await read(`${base}&offset=${rows.length}`);
    if (answer.kind === 'failed') return answer;
    const got = answer.data;
    if (got.offset !== rows.length || (total !== null && total !== got.total)) return { kind: 'failed', error: 'human-required view changed while paging; no complete count' };
    total = got.total;
    for (const job of got.rows) {
      if (ids.has(job.id)) return { kind: 'failed', error: 'human-required view repeated a job while paging; no complete count' };
      ids.add(job.id);
      rows.push(job);
    }
    if (got.cut !== null) return { kind: 'ready', data: { ...got, rows, cut: got.cut } };
    if (rows.length === total) return { kind: 'ready', data: { ...got, rows, offset: 0, cut: null } };
    if (got.rows.length === 0) return { kind: 'failed', error: 'human-required view ended before its total; no complete count' };
  }
  return { kind: 'ready', data: { rows, total: total ?? 0, limit: NEEDS_YOU_LIMIT, offset: 0, cut: `${rows.length} of at least ${total} jobs read — the four-page poll bound was reached` } };
}

/** Where the yard's one Release control lives: the HELD CARS lane of
 *  YardStatusPanel.svelte, which /it draws only inside the dock's panel
 *  (panel.ts STATION_BOARDS, opened by `?at=dock`, router.ts). A bare
 *  `/it#held-cars` opened no panel and showed no held car — measured by
 *  David on 2026-11-20, correcting design 58c72d95 D5. */
export const HELD_CARS_HREF = '/it?at=dock#held-cars';

/** Where a needs-you line opens, for both boards that read the view
 *  (this one and the station rail, station-answer.ts): a reserved
 *  release opens the held-car row, because the review step's page has
 *  no Release control and a second control would be a second copy of
 *  the rule's UI (design 58c72d95 D5); every other line opens its step
 *  page, with the board's own back-link `query`. */
export function needHref(job: Readonly<{ id: string }>, step: Readonly<{ id: string; act?: string }>, query = ''): string {
  if (step.act === 'release') return HELD_CARS_HREF;
  return `/ux/jobs/${encodeURIComponent(job.id)}/steps/${encodeURIComponent(step.id)}${query}`;
}

const RANK: Readonly<Record<string, number>> ={ emergency: 0, urgent: 1, standard: 2, scheduled: 3 };
export function needsYouBoard(needs: Remote<NeedsYou>, now: number): Board<NeedLine, Readonly<{ whom: string; cut: string | null }>> {
  if (needs.kind === 'loading') return { kind: 'unread', why: 'not read yet' };
  if (needs.kind === 'failed') return { kind: 'unread', why: `the read failed — ${needs.error}` };
  const { cut } = needs.data;
  const jobs = needs.data.rows.filter((job) => job.status === 'open' && job.steps.some((step) => step.human_required && ['ready', 'active'].includes(step.status)));
  const counted: Figure = cut !== null ? { kind: 'floor', text: String(jobs.length), troubled: false, why: cut }
    : jobs.length === 0 ? { kind: 'zero' } : { kind: 'value', text: String(jobs.length), troubled: false };
  const whom = 'IT · human-required jobs';
  if (jobs.length === 0) return { kind: 'empty', count: counted, words: cut === null ? 'No open IT jobs require a human.' : 'None of the jobs read requires a human, but the answer was cut — more may.', whom, cut };
  const ordered = [...jobs].sort((a, b) => {
    const aa = a.opened_on ?? ''; const bb = b.opened_on ?? '';
    return aa === bb ? (RANK[a.priority] ?? 3) - (RANK[b.priority] ?? 3) : !aa ? 1 : !bb ? -1 : aa.localeCompare(bb);
  });
  const today = new Date(now).toISOString().slice(0, 10);
  const links = ordered.flatMap((job) => job.steps.filter((step) => step.human_required && ['ready', 'active'].includes(step.status)).map((step): NeedLine => {
    const days = job.opened_on ? ageDays(job.opened_on, today) : null;
    return { id: step.id, href: needHref(job, step, `?${new URLSearchParams({ from: '/it', from_label: 'Department Map' })}`),
      step: step.kind, protocol: job.kind, title: job.title, stepTitle: step.title,
      priority: job.priority === 'standard' ? null : job.priority,
      age: days === null ? '?' : days === 0 ? 'today' : `${days}d` };
  }));
  return { kind: 'rows', count: counted, lines: links.slice(0, SHOWN), more: Math.max(0, links.length - SHOWN), whom, cut };
}
