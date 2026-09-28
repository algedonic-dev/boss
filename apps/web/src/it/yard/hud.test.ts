import { describe, expect, it } from 'bun:test';
import * as borders from './borders';
import * as hud from './hud';
import {
  countdownText,
  headerOf,
  machineHref,
  machinesBoard,
  needsYouBoard,
  nextUpBoard,
  outranksBoard,
  parseNeedsYou,
  type NeedsYou,
} from './hud';
import { parseRegions, type Regions } from './regions';
import type { Remote } from '../../data/remote';

// THE TOP BOARD (design ea906603, car 4 — backlog 74569e94) over the HUD
// frame of design 00774ca8. Four rows of things to act on replace the
// three rows of flow arithmetic, which David removed (Q5, 2026-09-27:
// "Let's just remove it for now … we can bring them back later if I
// miss them"). What these pin, row by row: each row is the server's
// field drawn as sent; each has three pictures — populated, EMPTY IN
// WORDS, and UNREAD with its reason — and an absent field (an older
// server) is unread, never empty; a failed read keeps no stale value.

const NOW = Date.parse('2026-09-27T12:00:04Z');
const READ_AT = Date.parse('2026-09-27T12:00:00Z');
const ZONE = 'America/Los_Angeles';

const outrank = (over: Record<string, unknown> = {}) => ({
  id: '0a8a2463-0000-4000-8000-000000000001',
  kind: 'backlog-item',
  title: 'Conductor merges on a red gate',
  priority: 'urgent',
  opened_at: '2026-09-24T09:00:00Z',
  age_minutes: 3 * 24 * 60 + 3 * 60,
  at: { slug: 'triage', title: 'Measure the claim, choose a route', status: 'ready', assignee_id: null },
  stations: ['backlog'],
  ...over,
});

const BASE = {
  window_hours: 24,
  now: '2026-09-27T12:00:00Z',
  regions: [],
  machines: {
    running: 12, idle: 11, failed: 0, unknown: 1, total: 24,
    failed_or_unknown: [{ region: 'marshalling', id: 'station:x', name: 'x', state: 'unknown', why: 'blind' }],
  },
};

const ready = (raw: Record<string, unknown>): Remote<Regions> => ({ kind: 'ready', data: parseRegions({ ...BASE, ...raw }) });

describe('OUTRANKS REGULAR ORDER — the server’s `outranks`, oldest first as sent', () => {
  it('lists each packet by priority, protocol and title with its age and where it stands, linking the packet', () => {
    const board = outranksBoard(
      ready({
        outranks: [
          outrank(),
          outrank({
            id: 'df6251c8-0000-4000-8000-000000000002', priority: 'emergency', title: 'Region comment links 404',
            age_minutes: 45, at: null, stations: null,
          }),
        ],
      }),
    );
    expect(board.kind).toBe('rows');
    if (board.kind !== 'rows') return;
    expect(board.count).toEqual({ kind: 'value', text: '2', troubled: true });
    expect(board.lines.map((l) => l.title)).toEqual(['Conductor merges on a red gate', 'Region comment links 404']);
    const [a, b] = board.lines;
    expect(a).toEqual({
      id: '0a8a2463-0000-4000-8000-000000000001',
      href: '/ux/jobs/0a8a2463-0000-4000-8000-000000000001',
      priority: 'urgent',
      protocol: 'backlog-item',
      title: 'Conductor merges on a red gate',
      age: '3d',
      at: 'triage',
      where: 'backlog',
    });
    // No workable step is said, and stations nobody read are not "nowhere".
    expect(b?.at).toBe('between steps');
    expect(b?.where).toBeNull();
    expect(b?.age).toBe('45m');
    // Never a bare uuid where a title belongs.
    for (const l of board.lines) expect(l.title).not.toMatch(/^[0-9a-f]{8}-/);
  });

  it('caps the list and counts the rest, rather than dropping it', () => {
    const many = Array.from({ length: 8 }, (_, i) => outrank({ id: `id-${i}`, title: `item ${i}` }));
    const board = outranksBoard(ready({ outranks: many }));
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines).toHaveLength(5);
    expect(board.more).toBe(3);
    expect(board.count).toEqual({ kind: 'value', text: '8', troubled: true });
  });

  it('an empty list is said in words — the answer, read', () => {
    expect(outranksBoard(ready({ outranks: [] }))).toEqual({
      kind: 'empty', count: { kind: 'zero' }, words: 'Nothing outranks regular order.',
    });
  });

  it('null (the server’s read failed) and absent (an older server) are unread, each with its reason — never empty', () => {
    const failed = outranksBoard(ready({ outranks: null }));
    expect(failed).toEqual({
      kind: 'unread', why: 'the server could not read what outranks regular order',
    });
    const older = outranksBoard(ready({}));
    expect(older.kind).toBe('unread');
    if (older.kind === 'unread') expect(older.why).toContain('this server does not send');
  });

  it('a malformed list is unread for that row alone — the regions read still stands', () => {
    const r = ready({ outranks: [{ id: 7 }] });
    expect(r.kind).toBe('ready');
    const board = outranksBoard(r);
    expect(board.kind).toBe('unread');
  });
});

describe('NEEDS YOU — /api/jobs/assignments?for=me, the viewer and every alias', () => {
  const row = (over: Record<string, unknown> = {}, step: Record<string, unknown> = {}) => ({
    job_id: 'b1ed7200-0000-4000-8000-000000000003',
    job_title: 'Merge the tenant main',
    opened_on: '2026-09-25',
    workflow: 'ship-a-change',
    subject_kind: 'custom',
    subject_id: 's',
    priority: 'standard',
    step: {
      id: 'c0ffee00-0000-4000-8000-000000000004', job_id: 'b1ed7200-0000-4000-8000-000000000003',
      kind: 'sign-off', title: 'Approve the merge', status: 'ready', assignee_id: 'emp-david', ...step,
    },
    ...over,
  });
  const FOR = { ids: ['emp-david'], roles: ['platform-admin'] };
  const needs = (data: unknown[], forWhom: unknown = FOR): Remote<NeedsYou> => ({
    kind: 'ready', data: parseNeedsYou({ data, total: data.length, for: forWhom }),
  });

  it('lists each step by kind, protocol and packet title, oldest first, linking the step', () => {
    const board = needsYouBoard(
      needs([
        row({ job_title: 'The newer one', opened_on: '2026-09-27' }, { id: 's-new', kind: 'review-design' }),
        row(),
        // Unassigned and role-matched is yours too.
        row({ job_title: 'A role step', opened_on: '2026-09-26', priority: 'urgent' }, { id: 's-role', assignee_id: null }),
      ]),
      NOW,
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines.map((l) => l.title)).toEqual(['Merge the tenant main', 'A role step', 'The newer one']);
    expect(board.lines[0]).toEqual({
      id: 'c0ffee00-0000-4000-8000-000000000004',
      href: '/ux/jobs/b1ed7200-0000-4000-8000-000000000003/steps/c0ffee00-0000-4000-8000-000000000004?from=%2Fit&from_label=Department+Map',
      step: 'sign-off',
      protocol: 'ship-a-change',
      title: 'Merge the tenant main',
      stepTitle: 'Approve the merge',
      priority: null,
      age: '2d',
    });
    expect(board.lines[1]?.priority).toBe('urgent');
    expect(board.lines[2]?.age).toBe('today');
    expect(board.count).toEqual({ kind: 'value', text: '3', troubled: false });
    expect(board.whom).toBe('emp-david · platform-admin');
  });

  it('a step someone else holds is theirs, not yours', () => {
    const board = needsYouBoard(needs([row({}, { assignee_id: 'agent-other' })]), NOW);
    expect(board.kind).toBe('empty');
  });

  it('nothing assigned is said in words, naming whom it read for', () => {
    expect(needsYouBoard(needs([]), NOW)).toEqual({
      kind: 'empty', count: { kind: 'zero' }, words: 'Nothing needs you.', whom: 'emp-david · platform-admin',
    });
  });

  it('a server that ignores for=me is unread — its empty answer is not "nothing needs you"', () => {
    // An older server drops the unknown parameter and answers the
    // no-selector empty list; only the `for` block says it was honoured.
    expect(() => parseNeedsYou({ data: [], total: 0 })).toThrow(/does not answer for=me/);
    expect(() => parseNeedsYou([])).toThrow();
  });

  it('a failed read, and one not landed yet, are unread with the reason', () => {
    expect(needsYouBoard({ kind: 'failed', error: '/api/jobs/assignments?for=me: HTTP 503' }, NOW)).toEqual({
      kind: 'unread', why: 'the read failed — /api/jobs/assignments?for=me: HTTP 503',
    });
    expect(needsYouBoard({ kind: 'loading' }, NOW)).toEqual({ kind: 'unread', why: 'not read yet' });
  });
});

describe('NEXT UP — a departures board, each time in the viewer’s timezone with a countdown', () => {
  // Car 3's wire shape, `boss_jobs::next_up::NextEvent` (WIP 5ff954c0):
  // kind kebab-case, `at` UTC or null, `estimate` drawn "~", `basis` the
  // small print, `unread` the reason a source could not be read.
  const ev = (kind: string, title: string, at: string | null, over: Record<string, unknown> = {}) => ({
    kind, title, at, estimate: false, basis: `basis of ${title}`, source: 'cadence registry + loading dock', unread: null, ...over,
  });

  it('prints each event’s time in the zone and the time left, in the server’s order, with its basis', () => {
    const board = nextUpBoard(
      ready({
        next_up: [
          ev('gates', 'next bay free', '2026-09-27T12:07:04Z', { estimate: true, basis: 'median gate 11m' }),
          ev('train-board', 'next board, 3 cars waiting', '2026-09-27T12:30:04Z'),
          ev('scheduled', 'daily publish-to-github', '2026-09-27T18:52:04Z'),
          ev('rotation-due', 'forge token (dev pod)', '2026-10-02T12:00:04Z'),
        ],
      }),
      NOW,
      ZONE,
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.zone).toBe('PDT');
    expect(board.lines.map((l) => [l.kind, l.title, l.time, l.countdown, l.estimate])).toEqual([
      ['gates', 'next bay free', '05:07', 'in 7m', true],
      ['train board', 'next board, 3 cars waiting', '05:30', 'in 30m', false],
      ['scheduled', 'daily publish-to-github', '11:52', 'in 6h52m', false],
      ['rotation due', 'forge token (dev pod)', 'Oct 02', 'in 5d', false],
    ]);
    expect(board.lines[0]?.basis).toBe('median gate 11m');
    expect(board.lines[0]?.state).toBe('timed');
  });

  it('an event with no time is listed with its basis; a dark source is its own unread line — the rest still read', () => {
    const board = nextUpBoard(
      ready({
        next_up: [
          ev('gates', 'a bay frees', '2026-09-27T12:10:04Z'),
          ev('train-board', 'next board', null, { basis: 'held: the track is occupied' }),
          ev('scheduled', 'dispatcher schedule', null, { unread: 'the dispatcher did not answer', basis: '', source: 'dispatcher schedule' }),
        ],
      }),
      NOW,
      ZONE,
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines[0]?.countdown).toBe('in 10m');
    expect(board.lines[1]).toMatchObject({ state: 'untimed', time: null, countdown: null, basis: 'held: the track is occupied' });
    expect(board.lines[2]).toMatchObject({ state: 'unread', time: null, why: 'the dispatcher did not answer', source: 'dispatcher schedule' });
  });

  it('null from the server is unread, not empty', () => {
    const b = nextUpBoard(ready({ next_up: null }), NOW, ZONE);
    expect(b.kind).toBe('unread');
  });

  it('empty is said in words; absent (car 3 not on this server) is unread, never empty', () => {
    expect(nextUpBoard(ready({ next_up: [] }), NOW, ZONE)).toEqual({
      kind: 'empty', count: { kind: 'zero' }, words: 'Nothing is due ahead.', zone: 'PDT',
    });
    const older = nextUpBoard(ready({}), NOW, ZONE);
    expect(older.kind).toBe('unread');
    if (older.kind === 'unread') expect(older.why).toContain('this server does not send');
  });

  it('counts down in words a reader can compare', () => {
    expect(countdownText(20_000)).toBe('now');
    expect(countdownText(59 * 60_000)).toBe('in 59m');
    expect(countdownText(65 * 60_000)).toBe('in 1h05m');
    expect(countdownText(49 * 3_600_000)).toBe('in 2d');
    expect(countdownText(-5 * 60_000)).toBe('5m late');
  });
});

describe('MACHINES — unchanged: the server’s count, failed and unjudged listed', () => {
  it('the cell is the server’s count, the unjudged listed by region', () => {
    const m = machinesBoard(ready({}));
    expect(m.failed).toEqual({ kind: 'zero' });
    expect(m.unjudged).toEqual({ kind: 'value', text: '1', troubled: false });
    expect(m.total).toEqual({ kind: 'value', text: '24', troubled: false });
    expect(m.listed.map((x) => `${x.region}/${x.id}`)).toEqual(['marshalling/station:x']);
    const failing = machinesBoard(ready({ machines: { ...BASE.machines, failed: 1 } }));
    expect(failing.failed).toEqual({ kind: 'value', text: '1', troubled: true });
  });

  it('an older server without the block is unanswered, never zero', () => {
    const m = machinesBoard(ready({ machines: null }));
    expect(m.total).toEqual({ kind: 'unread', why: 'the server sends no machine count' });
  });
});

describe('a failed read keeps no stale value, and the header names both times', () => {
  const failed: Remote<Regions> = { kind: 'failed', error: 'HTTP 502' };

  it('every regions-fed row is unread with the failure as its reason', () => {
    const why = 'the read failed — HTTP 502';
    expect(outranksBoard(failed)).toEqual({ kind: 'unread', why });
    expect(nextUpBoard(failed, NOW, ZONE)).toEqual({ kind: 'unread', why });
    const m = machinesBoard(failed);
    expect([m.failed, m.unjudged, m.total].every((f) => f.kind === 'unread')).toBe(true);
    expect(m.listed).toEqual([]);
  });

  it('the header: the read’s age, or the failure with the last good read', () => {
    const good: Remote<Regions> = ready({});
    expect(headerOf(good, READ_AT, NOW)).toEqual({ text: 'read 4s ago', read: 'ok' });
    const failedAt = Date.parse('2026-09-27T12:05:00Z');
    expect(headerOf(failed, failedAt, failedAt, { at: READ_AT })).toEqual({
      text: 'read failed 12:05Z · last good 12:00Z', read: 'failed',
    });
    expect(headerOf(failed, READ_AT, NOW, null).text).toBe('read failed 12:00Z · no good read yet');
    expect(headerOf({ kind: 'loading' }, null, NOW, null)).toEqual({ text: 'reading…', read: 'loading' });
  });
});

describe('what the board retires', () => {
  it('the three-thirds rows are gone (Q5), and the client-side border sum with them (decision 10)', () => {
    expect('hudOf' in hud).toBe(false);
    expect('labelOf' in hud).toBe(false);
    expect('summaryLine' in borders).toBe(false);
  });
});

// The host runners left receiving for the PLANT (62de32ae decision 11),
// and the server counts them in the machine cell under `plant`. The
// plant strip stands under the world map, so a listed plant machine
// opens the world — there is no /it/yard/plant to open.
describe('a listed machine opens where it is drawn', () => {
  it('a region machine opens its region, a plant machine the world', () => {
    const at = (region: string) => ({ region, id: 'x', name: 'x', state: 'failed' as const, why: '' });
    // A selection on the Department Map (design e765b3fc, car N1).
    expect(machineHref(at('marshalling'))).toBe('/it?at=marshalling');
    expect(machineHref(at('plant'))).toBe('/it');
  });
});
