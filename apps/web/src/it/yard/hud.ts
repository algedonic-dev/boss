// THE TOP BOARD — the HUD frame above the Department Map (design
// 00774ca8), redrawn by design ea906603 (backlog 74569e94, car 4). David,
// 2026-09-26: "update the Queue management, Actors building, and
// Delivery surface area on the display to something more useful …
// show urgent jobs on the top board with upcoming events for the IT
// department that are relevant, like the next train departure." So the
// three rows of flow arithmetic — which said THAT the balance moved,
// not what to do — are gone (Q5, David 2026-09-27: "Let's just remove
// it for now. I haven't been looking at those numbers."; the `thirds`
// payload is untouched and the phone strip still groups by it), and the
// frame draws four rows of things to act on:
//
//   OUTRANKS REGULAR ORDER  the server's `outranks` (car 1)
//   NEEDS YOU               /api/jobs/assignments?for=me (car 1)
//   NEXT UP                 the server's `next_up` (car 3,
//                           `boss_jobs::next_up::NextEvent`), a
//                           departures board: time in the viewer's
//                           timezone and a countdown, "~" on an
//                           estimate, the basis as small print
//   MACHINES                the `machines` cell, unchanged
//
// NOTHING HERE ADDS ANYTHING UP. Each row is a field the server wrote;
// this file turns it into a picture and caps a long list at a count of
// the rest. Each row is its own function so the frame can compute it
// INSIDE that row's error boundary (8c7c2f4b's intent): a row that
// cannot be drawn fails alone into unread, and the frame keeps its
// shape.
//
// THREE PICTURES PER ROW, and the difference is the point (the bar
// 17567423 set): `rows` — the answer, with lines; `empty` — the answer
// is none, SAID IN WORDS; `unread` — no answer, `?` in the dashed
// housing with the reason. An absent field (an older server), a null
// one (the server's own read failed) and a failed read are all unread,
// never empty — an empty list that reads "nothing" where nothing was
// read is the false-empty this frame exists to refuse.

import type { Remote } from '../../data/remote';
import { ageDays } from '../receiving/receiving';
import type { AssignmentRow } from '../../me/assignments';
import { regionHref, type MachineAt, type Regions } from './regions';

export type Figure =
  | Readonly<{ kind: 'value'; text: string; troubled: boolean }>
  | Readonly<{ kind: 'zero' }>
  | Readonly<{ kind: 'floor'; text: string; troubled: boolean; why: string }>
  | Readonly<{ kind: 'unread'; why: string }>;

/** A board row: its lines, capped, with a count of the rest; or none,
 *  in words; or unread, with why. */
export type Board<L, X = Readonly<Record<never, never>>> =
  | (Readonly<{ kind: 'rows'; count: Figure; lines: ReadonlyArray<L>; more: number }> & X)
  | (Readonly<{ kind: 'empty'; count: Figure; words: string }> & X)
  | Readonly<{ kind: 'unread'; why: string }>;

/** How many lines a row shows before it counts the rest — the frame
 *  stands above the map, so it may not push the map off the screen. */
export const SHOWN = 5;

const unread = (why: string): Figure => ({ kind: 'unread', why });

/** The reason every regions-fed row gives when the read did not land. */
function notRead(read: Remote<Regions>): string | null {
  if (read.kind === 'ready') return null;
  return read.kind === 'failed' ? `the read failed — ${read.error}` : 'not read yet';
}

/** A count as the frame prints it; zero is its own picture. */
function count(n: number, troubled: boolean): Figure {
  return n === 0 ? { kind: 'zero' } : { kind: 'value', text: String(n), troubled };
}

/** `45` minutes → `45m`, `300` → `5h`, `4500` → `3d`. */
function ageText(minutes: number): string {
  if (minutes < 60) return `${Math.max(0, minutes)}m`;
  const hours = Math.floor(minutes / 60);
  return hours < 48 ? `${hours}h` : `${Math.floor(hours / 24)}d`;
}

// ---------------------------------------------------------------------
// OUTRANKS REGULAR ORDER
// ---------------------------------------------------------------------

export type OutrankLine = Readonly<{
  id: string;
  href: string;
  priority: string;
  /** The packet's protocol — its kind. */
  protocol: string;
  title: string;
  age: string;
  /** Its workable step, or "between steps". */
  at: string;
  /** The stations it stands at, or null when they were not read. */
  where: string | null;
}>;

export function outranksBoard(read: Remote<Regions>): Board<OutrankLine> {
  const why = notRead(read);
  if (why !== null || read.kind !== 'ready') return { kind: 'unread', why: why ?? 'not read yet' };
  const field = read.data.outranks;
  if (field.kind === 'unread') return field;
  const all = field.rows;
  if (all.length === 0) {
    return { kind: 'empty', count: { kind: 'zero' }, words: 'Nothing outranks regular order.' };
  }
  // The server's order — oldest first — is the order drawn.
  const lines = all.slice(0, SHOWN).map((o) => ({
    id: o.id,
    href: `/ux/jobs/${encodeURIComponent(o.id)}`,
    priority: o.priority,
    protocol: o.kind,
    title: o.title,
    age: o.age_minutes === null ? '?' : ageText(o.age_minutes),
    at: o.at === null ? 'between steps' : (o.at.slug ?? o.at.title),
    where: o.stations === null ? null : o.stations.join(', '),
  }));
  // Anything here outranks the queue, so its count wears the troubled plate.
  return { kind: 'rows', count: count(all.length, true), lines, more: all.length - lines.length };
}

// ---------------------------------------------------------------------
// NEEDS YOU
// ---------------------------------------------------------------------

/** What `GET /api/jobs/assignments?for=me` answers: the viewer's
 *  workable steps and whom the server read for (design ea906603 Q2). */
export type NeedsYou = Readonly<{
  rows: ReadonlyArray<AssignmentRow>;
  for: Readonly<{ ids: ReadonlyArray<string>; roles: ReadonlyArray<string> }>;
}>;

/** The answer, or a throw. The `for` block is REQUIRED: a server older
 *  than car 1 ignores the unknown parameter and answers the no-selector
 *  empty list, and that empty list must not read "nothing needs you". */
export function parseNeedsYou(raw: unknown): NeedsYou {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    throw new Error('assignments: expected an object');
  }
  const o = raw as Record<string, unknown>;
  if (!Array.isArray(o.data)) throw new Error('assignments: expected a data list');
  const f = o.for;
  if (typeof f !== 'object' || f === null || !Array.isArray((f as { ids?: unknown }).ids)) {
    throw new Error('this server does not answer for=me (it predates design ea906603), so its empty list cannot say nothing needs you');
  }
  const whom = f as { ids: unknown[]; roles?: unknown };
  return {
    rows: o.data as AssignmentRow[],
    for: { ids: whom.ids.map(String), roles: Array.isArray(whom.roles) ? whom.roles.map(String) : [] },
  };
}

export const NEEDS_YOU_URL = '/api/jobs/assignments?for=me&limit=500';

export type NeedLine = Readonly<{
  id: string;
  href: string;
  /** The step's kind — sign-off, review-design, scope. */
  step: string;
  protocol: string;
  /** The PACKET's title; the step's own is `stepTitle`. */
  title: string;
  stepTitle: string;
  /** Shown only when it is not `standard`. */
  priority: string | null;
  age: string;
}>;

const PRIORITY_RANK: Readonly<Record<string, number>> = { emergency: 0, urgent: 1, standard: 2, scheduled: 3 };

/** Where a step opens, with the Back that returns here. */
const stepHref = (jobId: string, stepId: string): string =>
  `/ux/jobs/${encodeURIComponent(jobId)}/steps/${encodeURIComponent(stepId)}?${new URLSearchParams({ from: '/it', from_label: 'Department Map' })}`;

export function needsYouBoard(
  needs: Remote<NeedsYou>,
  now: number,
): Board<NeedLine, Readonly<{ whom: string }>> {
  if (needs.kind === 'loading') return { kind: 'unread', why: 'not read yet' };
  if (needs.kind === 'failed') return { kind: 'unread', why: `the read failed — ${needs.error}` };
  const { rows, for: whom } = needs.data;
  const ids = new Set(whom.ids);
  // A role-matched step someone else already holds is theirs, not yours.
  const yours = rows.filter((r) => {
    const holder = r.step.assignee_id ?? null;
    return (holder === null || ids.has(holder)) && (r.step.status === 'ready' || r.step.status === 'active');
  });
  const named = [...whom.ids, ...whom.roles].join(' · ');
  if (yours.length === 0) return { kind: 'empty', count: { kind: 'zero' }, words: 'Nothing needs you.', whom: named };
  // Oldest first (5877860d q2: a list ordered by priority buries what has
  // waited longest), priority breaking ties; an undated row sorts last.
  const ordered = [...yours].sort((a, b) => {
    const oa = a.opened_on ?? '';
    const ob = b.opened_on ?? '';
    if (oa !== ob) return !oa ? 1 : !ob ? -1 : oa.localeCompare(ob);
    return (PRIORITY_RANK[a.priority] ?? 3) - (PRIORITY_RANK[b.priority] ?? 3);
  });
  const today = new Date(now).toISOString().slice(0, 10);
  const lines = ordered.slice(0, SHOWN).map((r) => {
    const days = r.opened_on ? ageDays(r.opened_on, today) : null;
    return {
      id: r.step.id,
      href: stepHref(r.job_id, r.step.id),
      step: r.step.kind,
      protocol: r.workflow,
      title: r.job_title,
      stepTitle: r.step.title,
      priority: r.priority === 'standard' ? null : r.priority,
      age: days === null ? '?' : days === 0 ? 'today' : `${days}d`,
    };
  });
  return { kind: 'rows', count: count(yours.length, false), lines, more: yours.length - lines.length, whom: named };
}

// ---------------------------------------------------------------------
// NEXT UP — the departures board
// ---------------------------------------------------------------------

export type NextLine = Readonly<{
  /** `timed` — a time and a countdown; `untimed` — coming, with no time
   *  the record can support (`basis` says why); `unread` — the source
   *  could not be read (`why`). */
  state: 'timed' | 'untimed' | 'unread';
  /** The server's kind in words — `train-board` → `train board`. */
  kind: string;
  title: string;
  /** `05:30`, or `Oct 02` past a day away — in the viewer's zone. Null
   *  unless timed. */
  time: string | null;
  /** `in 30m`, `now`, `5m late`. Null with `time`. */
  countdown: string | null;
  /** The time is an estimate — drawn "~". */
  estimate: boolean;
  /** What the time rests on, or why there is none — the small print. */
  basis: string;
  /** The read the row came from. */
  source: string;
  /** Why the source is unread, when it is. */
  why: string | null;
}>;

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** A span as a departures board says it: `7m`, `6h52m`, `5d`. */
function span(ms: number): string {
  const m = Math.round(ms / MINUTE);
  if (m < 60) return `${m}m`;
  if (ms < 2 * DAY) return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}m`;
  return `${Math.round(ms / DAY)}d`;
}

/** The time left until a departure, from `ms` (negative once due). */
export function countdownText(ms: number): string {
  if (Math.abs(ms) < MINUTE) return 'now';
  return ms > 0 ? `in ${span(ms)}` : `${span(-ms)} late`;
}

/** The zone's short name for the column head — `PDT`. `timeZone`
 *  undefined is the viewer's own. */
function zoneName(now: number, timeZone: string | undefined): string {
  const parts = new Intl.DateTimeFormat('en-US', { timeZoneName: 'short', ...(timeZone ? { timeZone } : {}) }).formatToParts(
    new Date(now),
  );
  return parts.find((p) => p.type === 'timeZoneName')?.value ?? '';
}

function clockText(at: number, now: number, timeZone: string | undefined): string {
  const zone = timeZone ? { timeZone } : {};
  if (at - now < DAY) {
    return new Intl.DateTimeFormat('en-US', { hour: '2-digit', minute: '2-digit', hourCycle: 'h23', ...zone }).format(new Date(at));
  }
  return new Intl.DateTimeFormat('en-US', { month: 'short', day: '2-digit', ...zone }).format(new Date(at));
}

export function nextUpBoard(
  read: Remote<Regions>,
  now: number,
  timeZone?: string,
): Board<NextLine, Readonly<{ zone: string }>> {
  const why = notRead(read);
  if (why !== null || read.kind !== 'ready') return { kind: 'unread', why: why ?? 'not read yet' };
  const field = read.data.next_up;
  if (field.kind === 'unread') return field;
  const zone = zoneName(now, timeZone);
  if (field.rows.length === 0) return { kind: 'empty', count: { kind: 'zero' }, words: 'Nothing is due ahead.', zone };
  // The server's order is the board's: timed rows soonest first (capped
  // at six), then the untimed, then the unread sources.
  const lines = field.rows.map((e): NextLine => {
    const common = { kind: e.kind.replace(/-/g, ' '), title: e.title, estimate: e.estimate, basis: e.basis, source: e.source };
    if (e.unread !== null) {
      return { ...common, state: 'unread', time: null, countdown: null, why: e.unread };
    }
    const at = e.at === null ? Number.NaN : Date.parse(e.at);
    if (Number.isNaN(at)) return { ...common, state: 'untimed', time: null, countdown: null, why: null };
    return { ...common, state: 'timed', time: clockText(at, now, timeZone), countdown: countdownText(at - now), why: null };
  });
  return { kind: 'rows', count: count(lines.length, false), lines, more: 0, zone };
}

// ---------------------------------------------------------------------
// MACHINES — unchanged
// ---------------------------------------------------------------------

export type HudMachines = Readonly<{
  failed: Figure;
  unjudged: Figure;
  total: Figure;
  /** Each failed or unjudged machine, a link to its region's map. */
  listed: ReadonlyArray<MachineAt>;
}>;

/** A count the server gave, as a figure: zero is its own picture. */
function figure(v: number, troubled = false): Figure {
  return v === 0 ? { kind: 'zero' } : { kind: 'value', text: String(Math.round(v * 10) / 10), troubled };
}

export function machinesBoard(read: Remote<Regions>): HudMachines {
  const why = notRead(read) ?? (read.kind === 'ready' && read.data.machines === null ? 'the server sends no machine count' : null);
  if (why !== null || read.kind !== 'ready' || read.data.machines === null) {
    const w = why ?? 'not read yet';
    return { failed: unread(w), unjudged: unread(w), total: unread(w), listed: [] };
  }
  const m = read.data.machines;
  return { failed: figure(m.failed, true), unjudged: figure(m.unknown), total: figure(m.total), listed: m.failed_or_unknown };
}

/** Where a listed machine's link goes: its region, selected on the
 *  Department Map (design e765b3fc, car N1) — or, for a
 *  machine of the PLANT (design 62de32ae decision 11; the server's
 *  `thirds::PLANT`), the world, because the plant strip stands under the
 *  world map and serves every region rather than one. The plant is not
 *  a region, so the one region door (`regionHref`, backlog 594ffe96)
 *  already opens the world for it. */
export const machineHref = (m: MachineAt): string => regionHref(m.region);

// ---------------------------------------------------------------------
// THE HEADER
// ---------------------------------------------------------------------

/** The newest read that succeeded — its time, for the failure header.
 *  Its values are never drawn once a newer read failed: a stale figure
 *  shown as current is an answer where there should have been an error. */
export type LastGood = Readonly<{ at: number }>;

const hhmm = (ms: number): string => `${new Date(ms).toISOString().slice(11, 16)}Z`;

/** How long ago the read landed: `4s`, `3m`, `2h`. */
function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  return `${Math.floor(s / 3600)}h`;
}

/** "read 4s ago" — or the failure, with the last good read. `read` is
 *  `ok`, `loading` (nothing landed yet) or `failed`; only a failure is
 *  drawn as one. */
export function headerOf(
  read: Remote<Regions>,
  readAt: number | null,
  now: number,
  lastGood: LastGood | null = null,
): Readonly<{ text: string; read: 'ok' | 'loading' | 'failed' }> {
  if (read.kind === 'loading') return { text: 'reading…', read: 'loading' };
  if (read.kind === 'failed') {
    return {
      text: `read failed ${hhmm(readAt ?? now)} · ${lastGood ? `last good ${hhmm(lastGood.at)}` : 'no good read yet'}`,
      read: 'failed',
    };
  }
  return { text: `read ${ago(now - (readAt ?? now))} ago`, read: 'ok' };
}
