import { describe, expect, it } from 'bun:test';
import * as borders from './borders';
import * as hud from './hud';
import {
  countdownText,
  headerOf,
  machineHref,
  machinesBoard,
  NEEDS_YOU_LIMIT,
  NEEDS_YOU_URL,
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

  it('a claimed step says whether anyone is on it — the stalled ones say so (backlog 8e9d3723)', () => {
    const claimed = (id: string, run: unknown) =>
      outrank({ id, at: { slug: 'build', title: 'Build', status: 'active', assignee_id: 'agent-claude', run } });
    const board = outranksBoard(
      ready({
        outranks: [
          claimed('a', { state: 'building', run: '086d9793-92d8-402f' }),
          claimed('b', { state: 'never-started', run: 'f58a081d-4ce8', age_minutes: 37 * 60 }),
          claimed('c', { state: 'no-run' }),
          claimed('d', { state: 'unread' }),
          claimed('e', undefined),
        ],
      }),
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines.map((l) => l.at)).toEqual([
      'build · run 086d9793',
      'build · STALLED: run f58a081d briefed 37h ago, never started',
      'build · STALLED: claimed, no open run',
      'build · run unread',
      'build',
    ]);
  });

  it('a run inside its worker’s start bound reads starting, not stalled (backlog 57326714)', () => {
    const board = outranksBoard(
      ready({
        outranks: [
          outrank({
            id: 'f',
            at: {
              slug: 'build', title: 'Build', status: 'active', assignee_id: 'agent-claude',
              run: { state: 'starting', run: '0ccf6dab-4bd0', age_minutes: 5, expect_minutes: 15 },
            },
          }),
          outrank({
            id: 'g',
            at: {
              slug: 'build', title: 'Build', status: 'active', assignee_id: 'agent-claude',
              run: { state: 'starting', run: '1a2b3c4d-5e6f', age_minutes: 0, expect_minutes: 15 },
            },
          }),
        ],
      }),
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines.map((l) => l.at)).toEqual([
      'build · run 0ccf6dab starting: briefed 5m ago, 15m allowed',
      'build · run 1a2b3c4d starting: briefed just now, 15m allowed',
    ]);
  });

  it('a ready step nothing can dispatch is named undispatchable, with its row (backlog e2519eca)', () => {
    const readyStep = (id: string, slug: string, dispatch: unknown) =>
      outrank({ id, at: { slug, title: 'Step', status: 'ready', assignee_id: 'agent-claude', run: null, dispatch } });
    const board = outranksBoard(
      ready({
        outranks: [
          readyStep('a', 'prove-delivery', { state: 'undispatchable', kind: 'backlog-item', version: 15 }),
          readyStep('b', 'triage', { state: 'unread' }),
          readyStep('c', 'build', null),
          readyStep('d', 'build', undefined),
        ],
      }),
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines.map((l) => l.at)).toEqual([
      'prove-delivery · UNDISPATCHABLE: backlog-item v15 declares no agent block on prove-delivery and no person holds it',
      'triage · dispatch unread',
      'build',
      'build',
    ]);
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
      agentTakes: false,
    });
    expect(board.lines[1]?.priority).toBe('urgent');
    expect(board.lines[2]?.age).toBe('today');
    expect(board.count).toEqual({ kind: 'value', text: '3', troubled: false });
    expect(board.whom).toBe('emp-david · platform-admin');
  });

  it('a role step whose own agent block hands it to an agent says so on its line', () => {
    // Backlog 1dd6d7ad: the publish review step of 246d597a — a
    // checklist on David's role, carrying an analyst block — read as
    // waiting on him. The line now says an agent takes it.
    const board = needsYouBoard(
      needs([
        row({}, { id: 's-agent', kind: 'checklist', assignee_id: null, agent_takes: true }),
        row({}, { id: 's-his', kind: 'checklist', assignee_id: null, agent_takes: false }),
      ]),
      NOW,
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines.map((l) => [l.id, l.agentTakes])).toEqual([
      ['s-agent', true],
      ['s-his', false],
    ]);
  });

  it('a step someone else holds is theirs, not yours', () => {
    const board = needsYouBoard(needs([row({}, { assignee_id: 'agent-other' })]), NOW);
    expect(board.kind).toBe('empty');
  });

  it('nothing assigned is said in words, naming whom it read for', () => {
    expect(needsYouBoard(needs([]), NOW)).toEqual({
      kind: 'empty', count: { kind: 'zero' }, words: 'Nothing needs you.', whom: 'emp-david · platform-admin', cut: null,
    });
  });

  it('a server that ignores for=me is unread — its empty answer is not "nothing needs you"', () => {
    // An older server drops the unknown parameter and answers the
    // no-selector empty list; only the `for` block says it was honoured.
    expect(() => parseNeedsYou({ data: [], total: 0 })).toThrow(/does not answer for=me/);
    expect(() => parseNeedsYou([])).toThrow();
  });

  // Backlog 97b5cf45: the server says when its answer was cut
  // (`truncated`, with `total` counted before the cut — 09074ada), and
  // the row counted only the rows it got, so a cut queue read as a
  // complete, smaller one.
  it('a cut answer counts a floor and says it was cut — never a whole, smaller queue', () => {
    const cut = (raw: Record<string, unknown>): Remote<NeedsYou> => ({ kind: 'ready', data: parseNeedsYou({ for: FOR, ...raw }) });
    const two = [row({}, { id: 's-1' }), row({}, { id: 's-2' })];
    const board = needsYouBoard(cut({ data: two, total: 7, truncated: true }), NOW);
    if (board.kind !== 'rows') throw new Error(board.kind);
    const why = '2 of at least 7 rows read — the answer was cut at its limit';
    expect(board.cut).toBe(why);
    expect(board.count).toEqual({ kind: 'floor', text: '2', troubled: false, why });
    // The flag alone is enough: a store read that came back full makes
    // the count a floor even when the rows fit.
    const flagged = needsYouBoard(cut({ data: two, total: 2, truncated: true }), NOW);
    if (flagged.kind !== 'rows') throw new Error(flagged.kind);
    expect(flagged.cut).toBe('2 rows read and the answer was cut at its limit — how many more is unknown');
    expect(flagged.count.kind).toBe('floor');
    // A whole answer is a plain count, and says nothing about a cut.
    const whole = needsYouBoard(cut({ data: two, total: 2, truncated: false }), NOW);
    if (whole.kind !== 'rows') throw new Error(whole.kind);
    expect(whole.cut).toBeNull();
    expect(whole.count).toEqual({ kind: 'value', text: '2', troubled: false });
  });

  it('a server older than the flag is cut when its rows fill the page, or it sends no total', () => {
    const full = Array.from({ length: NEEDS_YOU_LIMIT }, (_, i) => row({}, { id: `s-${i}` }));
    expect(parseNeedsYou({ data: full, total: NEEDS_YOU_LIMIT, for: FOR }).cut).toMatch(/cut at its limit/);
    expect(parseNeedsYou({ data: [row()], for: FOR }).cut).toMatch(/cut at its limit/);
    expect(parseNeedsYou({ data: [row()], total: 1, for: FOR }).cut).toBeNull();
    expect(NEEDS_YOU_URL).toBe(`/api/jobs/assignments?for=me&limit=${NEEDS_YOU_LIMIT}`);
  });

  it('a cut answer with none of its read rows yours does not say nothing needs you', () => {
    const board = needsYouBoard(
      { kind: 'ready', data: parseNeedsYou({ data: [row({}, { assignee_id: 'agent-other' })], total: 4, truncated: true, for: FOR }) },
      NOW,
    );
    if (board.kind !== 'empty') throw new Error(board.kind);
    expect(board.words).toBe('None of the rows read needs you, but the answer was cut — more may.');
    expect(board.count).toEqual({ kind: 'floor', text: '0', troubled: false, why: '1 of at least 4 rows read — the answer was cut at its limit' });
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

  // Backlog bd506215: a source the server WITHHELD from a narrowed scope
  // (0964ba80 — `withheld_rows` in next_up.rs) is a refusal, not a dark
  // source: its own state, not the unread `?` a failure wears.
  it('a source withheld by policy scope is its own neutral line, never the unread ?', () => {
    const why = "withheld: this caller's policy scope does not read every packet, and this source is not scoped by packet";
    const board = nextUpBoard(
      ready({
        next_up: [
          ev('gates', 'a bay frees', '2026-09-27T12:10:04Z'),
          ev('scheduled', 'scheduled rules', null, { unread: why, withheld: true, basis: 'the dispatcher schedule is withheld from this caller', source: 'dispatcher schedule' }),
          ev('scheduled', 'dispatcher schedule', null, { unread: 'the dispatcher did not answer', basis: '', source: 'dispatcher schedule' }),
          // The refusal's words WITHOUT the server's flag (1805bac0): the
          // words decide nothing, so this is a source that did not answer.
          ev('rotation-due', 'credential rotations', null, { unread: why, basis: '', source: 'credentials registry' }),
        ],
      }),
      NOW,
      ZONE,
    );
    if (board.kind !== 'rows') throw new Error(board.kind);
    expect(board.lines[1]).toMatchObject({ state: 'withheld', title: 'scheduled rules', time: null, countdown: null, why });
    // The control: a source that failed is still unread.
    expect(board.lines[2]?.state).toBe('unread');
    expect(board.lines[3]?.state).toBe('unread');
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
