import { describe, expect, test } from 'bun:test';
import { parseStationLoad, type StationLoadRow } from '../marshalling/marshalling';
import { claimsRead, heldClaim, treeRead } from '../../../tests/fixtures/station-tree';
import { NEEDS_YOU_LIMIT, needHref, type NeedsYou } from '../yard/human-needs';
import type { Remote } from '../../data/remote';
import { parseTree, type StationTree } from './station-tree';
import { parsePageClaims, type PageClaims } from './station-page';
import {
  answerBand, bindingPlate, heldKinds, needsBand, needsYouList, needsYouUrl, stuckList, subtitleOf, waitsLadder,
} from './station-answer';

// THE ANSWER BAND AND THE RIGHT RAIL (design 08d1530e, car A, decided
// items 1, 6 and 7; backlog 950ad972). The header answers in five
// seconds — working, stuck, needs you, the binding constraint — and the
// rail ranks the waits worst first. Every figure is a read's; a figure
// no read states is said unread, never computed.

const NOW = Date.parse('2026-11-10T07:00:00Z');
const tree = (): StationTree => parseTree(treeRead());
type Load = Remote<ReadonlyMap<string, StationLoadRow>>;
const load = (rows: ReadonlyArray<Readonly<Record<string, unknown>>> = [
  { station: 'gates', kind: 'group', workflow_kind: 'gate-run', depth: 3, oldest_age_days: 0.4, also_elsewhere: 0 },
  { station: 'engineering', kind: 'group', workflow_kind: null, depth: 0, oldest_age_days: null, also_elsewhere: 0 },
  { station: 'loading-dock', kind: 'group', workflow_kind: 'ship-a-change', depth: 30, oldest_age_days: 2.5, also_elsewhere: 0 },
  { station: 'design-review', kind: 'group', workflow_kind: 'design-doc', depth: 4, oldest_age_days: 9, also_elsewhere: 0 },
]): Load => ({ kind: 'ready', data: new Map(parseStationLoad({ data: rows, distinct_packets: 33 }).map((r) => [r.station, r] as const)) });

const PLACED = [
  heldClaim('j-gate-1', 'gates', { holder: 'automation:gate-runner', executor_kind: 'automaton' }),
  heldClaim('j-build-1', 'engineering'),
  heldClaim('j-ci-1', 'forge-ci', { holder: 'automation:forge-actions-runner', executor_kind: 'automaton' }),
  heldClaim('j-david', 'design-review', { holder: 'emp-david', executor_kind: 'person' }),
];
const PLACELESS = heldClaim('j-old', null, { holder: 'automation:train-conductor', since: '2026-11-01T07:00:00Z' });
const claims = (held: ReadonlyArray<Readonly<Record<string, unknown>>>, extra: Readonly<Record<string, unknown>> = {}): Remote<PageClaims> =>
  ({ kind: 'ready', data: parsePageClaims({ ...claimsRead(held), ...extra }) });

describe('the title said once', () => {
  test('a title that repeats the station name loses it; one that does not keeps it', () => {
    expect(subtitleOf('engineering', 'Engineering — makes the change')).toBe('makes the change');
    expect(subtitleOf('gates', 'gates - where a car is judged')).toBe('where a car is judged');
    expect(subtitleOf('engineering', 'Engineering')).toBeNull();
    expect(subtitleOf('forge', 'The forge and its runners')).toBe('The forge and its runners');
  });
});

describe('the waits ladder: not admitted, worst first', () => {
  test('an idle machine with a queue is stuck and ranks first; a held seat is admitted', () => {
    const l = waitsLadder(tree(), 'engineering', load(), claims(PLACED));
    if (l.kind !== 'rungs') throw new Error('ladder unread');
    expect(l.rungs.map((r) => [r.station, r.state, r.waiting, r.notAdmitted, r.oldest])).toEqual([
      ['loading-dock', 'stuck', 30, 30, '2 d'],
      ['gates', 'waiting', 3, 2, '9 h'],
    ]);
    expect(l.rungs[0]?.words).toBe('Its seat is idle while 30 wait.');
    expect(l.rungs[1]?.words).toBe('1 of 2 seats held.');
    expect(l.rungs[0]?.href).toBe('/it/tree/loading-dock');
    // design-review stands under executive, not here.
    expect(l.rungs.some((r) => r.station === 'design-review')).toBe(false);
    // The budget is said once, never invented per rung.
    expect(l.caveat).toContain('budget');
  });

  test('a claim the record cannot place could hold any seat, so no seat is judged idle', () => {
    const l = waitsLadder(tree(), 'engineering', load(), claims([...PLACED, PLACELESS]));
    if (l.kind !== 'rungs') throw new Error('ladder unread');
    expect(l.rungs.every((r) => r.state === 'waiting')).toBe(true);
    expect(l.rungs.find((r) => r.station === 'loading-dock')?.words).toBe('Seats busy not recorded: 1 claim stands at no station.');
  });

  test('a failed claims read keeps the queue depths and admits nothing it cannot read', () => {
    const l = waitsLadder(tree(), 'engineering', load(), { kind: 'failed', error: '/api/claims: HTTP 503' });
    if (l.kind !== 'rungs') throw new Error('ladder unread');
    expect(l.rungs.map((r) => [r.station, r.notAdmitted, r.state])).toEqual([['loading-dock', null, 'waiting'], ['gates', null, 'waiting']]);
    expect(l.rungs[0]?.words).toBe('Claims unread, so admission is not told apart.');
  });

  test('a failed load read is the ladder unread, with why', () => {
    expect(waitsLadder(tree(), 'engineering', { kind: 'failed', error: '/api/stations/load: HTTP 502' }, claims(PLACED)))
      .toEqual({ kind: 'unread', why: 'the load read failed: /api/stations/load: HTTP 502' });
    expect(waitsLadder(tree(), 'engineering', { kind: 'loading' }, claims(PLACED))).toEqual({ kind: 'unread', why: 'reading the load' });
  });

  test('a refused load read says it was refused, with the server reason (7abb184f)', () => {
    const error = '/api/stations/load: HTTP 403: emp-x (role intern) may read no packets, so no station can be evaluated for them';
    expect(waitsLadder(tree(), 'engineering', { kind: 'failed', error, refused: true }, claims(PLACED)))
      .toEqual({ kind: 'unread', why: `the load read was refused: ${error}` });
  });
});

describe('the stuck list', () => {
  test('stuck stations and claims at no station, each with its age', () => {
    const s = stuckList(tree(), 'engineering', load(), claims([...PLACED, PLACELESS]), NOW);
    if (s.kind !== 'items') throw new Error('stuck unread');
    expect(s.items.map((i) => [i.words, i.age])).toEqual([['1 claim at no station', '9 d']]);
    const t = stuckList(tree(), 'engineering', load(), claims(PLACED), NOW);
    if (t.kind !== 'items') throw new Error('stuck unread');
    expect(t.items.map((i) => [i.words, i.age, i.href])).toEqual([['loading-dock: 30 not admitted, its seat idle', '2 d', '/it/tree/loading-dock']]);
  });

  test('an unread claims read is the list unread', () => {
    expect(stuckList(tree(), 'engineering', load(), { kind: 'failed', error: 'x' }, NOW)).toEqual({ kind: 'unread', why: 'the claims read failed: x' });
  });
});

describe('the binding constraint is the claims read’s own rule', () => {
  const pools = [
    { pool_key: 'host:w-1', pool: { name: 'host:w-1', unit: 'vCPU', capacity: { state: 'configured', amount: '32' } }, reserved: '7.5', failed_reservations: 1, releases: 139, state: 'attention', band: null },
    { pool_key: 'robots:opus', pool: { name: 'robots:opus', unit: 'slot', capacity: { state: 'unknown' } }, reserved: '2', failed_reservations: 0, releases: 3, state: 'attention', band: null },
  ];
  test('the pool the server names, its reserved figure and its refusals', () => {
    expect(bindingPlate(claims(PLACED, { binding: 'host:w-1', pools, window_hours: 24 }))).toEqual({
      kind: 'pool', pool: 'host:w-1', figure: '7.5 of 32 vCPU reserved', refused: '1 reservation refused in 24 h',
    });
  });
  test('none named is nothing refused in the window; no key is unread', () => {
    expect(bindingPlate(claims(PLACED, { binding: null, pools, window_hours: 24 }))).toEqual({ kind: 'none', words: 'no reservation refused in 24 h' });
    // A server that predates the binding key: unread, never "none".
    expect(bindingPlate({ kind: 'ready', data: parsePageClaims({ claims: PLACED }) })).toEqual({ kind: 'unread', why: 'the claims read names no binding' });
    expect(bindingPlate({ kind: 'failed', error: 'down' })).toEqual({ kind: 'unread', why: 'the claims read failed: down' });
  });
  test('a binding pool the read carries no reading for says capacity unknown', () => {
    expect(bindingPlate(claims(PLACED, { binding: 'disk:x', pools, window_hours: 24 }))).toEqual({
      kind: 'pool', pool: 'disk:x', figure: 'reserved unread', refused: 'refusals unread',
    });
  });
});

describe('needs you: the human-required view, over the protocols this station holds', () => {
  test('the protocols are the ones its stations queue; the root asks for all', () => {
    expect(heldKinds(tree(), 'engineering', load())).toEqual({ kind: 'kinds', kinds: ['gate-run', 'ship-a-change'] });
    expect(heldKinds(tree(), 'company', load())).toEqual({ kind: 'all' });
    expect(heldKinds(tree(), 'forge', load())).toEqual({ kind: 'none', why: 'no station here queues a protocol' });
    expect(heldKinds(tree(), 'engineering', { kind: 'loading' })).toEqual({ kind: 'unread', why: 'reading the load' });
  });
  test('the read names the kinds as the server parses them, a JSON array', () => {
    expect(needsYouUrl({ kind: 'all' })).toBe(`/api/jobs?status=open&human_required_view=true&limit=${NEEDS_YOU_LIMIT}`);
    expect(needsYouUrl({ kind: 'kinds', kinds: ['gate-run', 'ship-a-change'] }))
      .toBe(`/api/jobs?status=open&human_required_view=true&limit=${NEEDS_YOU_LIMIT}&kinds=${encodeURIComponent('["gate-run","ship-a-change"]')}`);
  });
  const job = (id: string, status: string, opened = '2026-11-08') => ({
    id, kind: 'ship-a-change', title: `packet ${id}`, status: 'open', priority: 'standard', opened_on: opened,
    steps: [{ id: `st-${id}`, kind: 'sign-off', title: 'Sign it off', status, human_required: true }],
  });
  const needs = (rows: NeedsYou['rows'], cut: string | null = null): Remote<NeedsYou> =>
    ({ kind: 'ready', data: { rows, total: rows.length, limit: NEEDS_YOU_LIMIT, offset: 0, cut } });
  test('only a ready or active human-required step needs you; a cut answer is a floor', () => {
    const l = needsYouList(needs([job('a', 'ready'), job('b', 'pending')]), NOW);
    expect(l.count).toEqual({ kind: 'count', n: 1, floor: false });
    if (l.kind !== 'lines') throw new Error('no lines');
    expect(l.lines).toEqual([{ id: 'st-a', title: 'packet a', step: 'Sign it off', age: '2 d', href: '/ux/jobs/a/steps/st-a' }]);
    expect(needsYouList(needs([job('a', 'active')], '1 of at least 900 jobs read'), NOW).count).toEqual({ kind: 'count', n: 1, floor: true });
    expect(needsYouList({ kind: 'failed', error: 'HTTP 503' }, NOW)).toEqual({ kind: 'unread', why: 'the human-required read failed: HTTP 503', count: { kind: 'unread', why: 'the human-required read failed: HTTP 503' } });
  });
  test('a reserved release on the rail opens the held-car row, the href the IT board uses (design 58c72d95 D5)', () => {
    const rv = { id: 'rv', kind: 'release', title: 'Release at 9e1576b8, reserved for emp-david', status: 'active', human_required: true, act: 'release', reserved_for: 'emp-david' };
    const car = { ...job('car', 'active'), steps: [rv] };
    const l = needsYouList(needs([car, job('a', 'ready')]), NOW);
    if (l.kind !== 'lines') throw new Error('no lines');
    expect(l.count).toEqual({ kind: 'count', n: 2, floor: false });
    expect(l.lines.map((line) => [line.id, line.href])).toEqual([['rv', '/it?at=dock#held-cars'], ['st-a', '/ux/jobs/a/steps/st-a']]);
    expect(l.lines.map((line) => line.href)).toEqual([needHref(car, rv), needHref({ id: 'a' }, { id: 'st-a' })]);
  });
  test('the band says the count in words, a floor as at least, and unread with why', () => {
    expect(needsBand(needsYouList(needs([job('a', 'ready')]), NOW))).toEqual({ kind: 'count', n: 1, floor: false, words: 'need you: 1 human-required step' });
    expect(needsBand(needsYouList(needs([]), NOW))).toEqual({ kind: 'count', n: 0, floor: false, words: 'need you: no human-required step here' });
    expect(needsBand(needsYouList(needs([job('a', 'ready'), job('b', 'active')], 'cut'), NOW))).toMatchObject({ floor: true, words: 'need you: at least 2 human-required steps' });
    expect(needsBand(needsYouList({ kind: 'loading' }, NOW))).toEqual({ kind: 'unread', why: 'reading the human-required view' });
  });
});

describe('the answer band', () => {
  test('working counts the claims placed inside, by part; stuck counts the stuck list', () => {
    const b = answerBand(tree(), 'engineering', load(), claims([...PLACED, PLACELESS]), NOW);
    expect(b.working).toEqual({ kind: 'count', n: 3, floor: false, words: 'working: 1 at engineering itself, 1 at forge, 1 at gates' });
    expect(b.stuck).toEqual({ kind: 'count', n: 1, floor: false, words: 'stuck: 1 claim at no station' });
    const idle = answerBand(tree(), 'engineering', load(), claims(PLACED), NOW);
    expect(idle.stuck).toEqual({ kind: 'count', n: 1, floor: false, words: 'stuck: loading-dock: 30 not admitted, its seat idle' });
  });
  test('nothing working and nothing stuck are said, and a failed read is unread', () => {
    const b = answerBand(tree(), 'probe-shed', load(), claims([]), NOW);
    expect(b.working).toEqual({ kind: 'count', n: 0, floor: false, words: 'working: nothing claimed here' });
    expect(b.stuck).toEqual({ kind: 'count', n: 0, floor: false, words: 'stuck: nothing judged stuck' });
    const f = answerBand(tree(), 'engineering', load(), { kind: 'failed', error: 'HTTP 503' }, NOW);
    expect(f.working).toEqual({ kind: 'unread', why: 'the claims read failed: HTTP 503' });
    expect(f.stuck).toEqual({ kind: 'unread', why: 'the claims read failed: HTTP 503' });
  });
});
