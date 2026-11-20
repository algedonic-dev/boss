import { describe, expect, test } from 'bun:test';
import { parseStationFlow, parseStationLoad, type StationFlowEnvelope, type StationLoadRow } from '../marshalling/marshalling';
import { TREE_ROWS, claimsRead, heldClaim, queueRead, treeRead } from '../../../tests/fixtures/station-tree';
import type { Remote } from '../../data/remote';
import { parseTree, type StationTree } from './station-tree';
import { parsePageClaims } from './station-page';
import {
  HEAD_MAX, STILL_BOUND, admitOutbound, declaredRoutes, floorModel, layoutFloor, parseFloorEvent, parseQueue, peerOf, pileOf,
  pileThickness, queuesToRead, stillOf, type FloorReads, type QueueRead,
} from './station-floor';

// THE STATION FLOOR (backlog dd0eb058, David's decisions on the prototype
// canvas 2026-11-19): one admission edge, no Out (Outbound is a record
// naming the next station at the same tier), fixed places, a job a token
// that moves only when its record moves, the head as tokens and the rest
// as an age-banded pile, stillness orange past its bound, and no figure
// no read states.

const NOW = Date.parse('2026-11-19T12:00:00Z');
const DAY = 86_400_000;
const ago = (days: number): string => new Date(NOW - days * DAY).toISOString();
const tree = (rows = TREE_ROWS): StationTree => parseTree(treeRead(undefined, rows));
/** Engineering with a route out: the probe shed sends on to converge,
 *  a part of operations. */
const OUT_ROWS = TREE_ROWS.map((r) => (r.name === 'probe-shed' ? { ...r, edges: ['converge'] } : r));

const loadOf = (rows: ReadonlyArray<Readonly<Record<string, unknown>>>): Remote<ReadonlyMap<string, StationLoadRow>> => ({
  kind: 'ready',
  data: new Map(parseStationLoad({ data: rows, distinct_packets: 0 }).map((r) => [r.station, r] as const)),
});
const flowOf = (rows: ReadonlyArray<readonly [string, number, number]>): Remote<StationFlowEnvelope> => ({
  kind: 'ready',
  data: parseStationFlow({
    basis: 'step-window-v1', window_hours: 24, as_of: '2026-11-19T12:00:00Z',
    data: rows.map(([station, arrived, departed]) => ({ station, kind: 'group', basis: 'step-window-v1', arrived, departed, served: departed, net: arrived - departed, unavailable_reason: null })),
  }),
});
const queue = (station: string, members: Parameters<typeof queueRead>[1]): Remote<QueueRead> => ({ kind: 'ready', data: parseQueue(queueRead(station, members)) });

const reads = (over: Partial<FloorReads> = {}): FloorReads => ({
  load: loadOf([
    { station: 'gates', kind: 'group', workflow_kind: 'gate-run', depth: 6, oldest_age_days: 40, also_elsewhere: 0 },
    { station: 'loading-dock', kind: 'group', workflow_kind: 'ship-a-change', depth: 0, oldest_age_days: null, also_elsewhere: 0 },
  ]),
  claims: { kind: 'ready', data: parsePageClaims(claimsRead([heldClaim('j-held', 'gates', { since: ago(0.5), holder: 'automation:gate-runner', executor_kind: 'automaton' })])) },
  flow: flowOf([['engineering', 5, 4], ['gates', 3, 2], ['loading-dock', 2, 0], ['design', 0, 0], ['forge', 1, 1], ['probe-shed', 0, 0]]),
  queues: new Map([['gates', queue('gates', [
    { id: 'j-held', title: 'Held at the gates', openedAt: ago(2) },
    { id: 'j-1', title: 'First in line', openedAt: ago(0.2) },
    { id: 'j-2', title: 'Second', openedAt: ago(9) },
    { id: 'j-3', title: 'Third', openedAt: ago(3) },
    { id: 'j-4', title: 'Fourth', openedAt: ago(0.5) },
    { id: 'j-5', title: 'Fifth', openedAt: ago(40) },
    { id: 'j-6', title: 'Sixth', openedAt: ago(12) },
  ])]]),
  movedJobs: new Map(),
  movedStations: new Map(),
  ...over,
});

describe('the queue read', () => {
  test('members in the station’s own order; a packet’s last move is its newest step completion, else its opening', () => {
    const q = parseQueue({ ...queueRead('gates', [{ id: 'a', title: 'A', openedAt: '2026-11-01T00:00:00Z' }, { id: 'b', title: 'B' }]), steps: { a: [{ completed_at: '2026-11-10T00:00:00Z' }, { completed_at: null }] } });
    expect(q.members.map((m) => m.id)).toEqual(['a', 'b']);
    expect(q.members[0]?.movedAt).toBe('2026-11-10T00:00:00Z');
    expect(q.members[1]?.movedAt).toBe('2026-11-08T05:00:00Z');
    expect(q.total).toBe(2);
  });

  test('a body that is not the envelope is refused, never an empty queue', () => {
    expect(() => parseQueue({ rows: [] })).toThrow('queue read');
  });
});

describe('the pile behind the head', () => {
  test('four age bands — under a day, 1 to 7, 8 to 30, over 30 — and an age the read lacks said apart', () => {
    const members = [0.5, 3, 7.5, 10, 30, 31, 45].map((d, i) => ({ id: `p${i}`, title: `p${i}`, priority: null, openedAt: ago(d), movedAt: ago(d) }));
    const pile = pileOf([...members, { id: 'x', title: 'x', priority: null, openedAt: null, movedAt: null }], NOW);
    expect(pile?.bands).toEqual([1, 2, 2, 2]);
    expect(pile?.unaged).toBe(1);
    expect(pile?.words).toBe('8 behind the head: 2 over 30 days, 2 at 8 to 30 days, 1 of unread age');
    expect(pileOf([], NOW)).toBeNull();
  });

  test('thickness grows with depth and stays bounded', () => {
    expect(pileThickness(0)).toBe(0);
    expect(pileThickness(1)).toBe(8);
    expect(pileThickness(49)).toBeGreaterThan(pileThickness(9));
    expect(pileThickness(886)).toBe(40);
  });
});

describe('stillness', () => {
  test('how long since the record moved, orange past its bound, and unrecorded said so', () => {
    expect(stillOf(ago(0.0001), NOW, STILL_BOUND.working)).toEqual({ ms: 8640, words: 'just moved', past: false });
    expect(stillOf(ago(2), NOW, STILL_BOUND.working)).toEqual({ ms: 2 * DAY, words: 'still 2d', past: true });
    expect(stillOf(ago(2), NOW, STILL_BOUND.waiting).past).toBe(false);
    expect(stillOf(null, NOW, STILL_BOUND.station)).toEqual({ ms: null, words: 'still: not recorded', past: false });
  });
});

describe('peers and the Outbound record', () => {
  test('a peer is the station at the same tier, never a part inside it', () => {
    const t = tree();
    expect(peerOf(t, 'engineering', 'converge')).toBe('operations');
    expect(peerOf(t, 'gates', 'converge')).toBe('operations');
    expect(peerOf(t, 'gates', 'loading-dock')).toBe('loading-dock');
    expect(peerOf(t, 'engineering', 'gates')).toBeNull();
    expect(peerOf(t, 'engineering', 'nowhere')).toBeNull();
  });

  test('a packet on the floor claimed outside it is recorded as sent to that peer; a close as left; nothing else', () => {
    const t = tree();
    const onFloor = new Map([['j-1', 'First in line']]);
    const ev = (id: string, over: Readonly<Record<string, unknown>>) => parseFloorEvent(JSON.stringify({ event_id: id, timestamp: '2026-11-19T11:59:00Z', source: 'boss-jobs-api', kind: 'jobs.step.updated', payload: { job_id: 'j-1', _station: 'converge' }, ...over }))!;
    let list = admitOutbound([], ev('e1', {}), t, 'engineering', onFloor);
    expect(list).toEqual([{ key: 'e1', at: '2026-11-19T11:59:00Z', title: 'First in line', href: '/ux/jobs/j-1', to: 'operations', words: 'sent to operations' }]);
    list = admitOutbound(list, ev('e1', {}), t, 'engineering', onFloor);
    expect(list).toHaveLength(1);
    // A claim inside the floor is a move on it, not a departure.
    expect(admitOutbound([], ev('e2', { payload: { job_id: 'j-1', _station: 'gates' } }), t, 'engineering', onFloor)).toEqual([]);
    // A packet this floor never held is not its departure.
    expect(admitOutbound([], ev('e3', { payload: { job_id: 'j-9', _station: 'converge' } }), t, 'engineering', onFloor)).toEqual([]);
    const closed = ev('e4', { kind: 'jobs.job.status_changed', payload: { id: 'j-1', old_status: 'open', new_status: 'closed' } });
    expect(admitOutbound([], closed, t, 'engineering', onFloor)[0]?.words).toBe('closed: left the floor');
  });

  test('the stream: a claim, a completion, a release and a status change move a token; any other row does not', () => {
    const frame = (kind: string, payload: Readonly<Record<string, unknown>>) => JSON.stringify({ event_id: 'e', timestamp: '2026-11-19T11:59:00Z', source: 's', kind, payload });
    expect(parseFloorEvent(frame('jobs.step.completed', { job_id: 'j-1', step_id: 's' }))).toMatchObject({ kind: 'jobs.step.completed', job: 'j-1', station: null });
    expect(parseFloorEvent(frame('jobs.station.claimed', { receipt_json: JSON.stringify({ station: { name: 'gates' }, selected: { job_id: 'j-2' } }) }))).toMatchObject({ job: 'j-2', station: 'gates' });
    expect(parseFloorEvent(frame('jobs.step.updated', { job_id: 'j-1' }))).toBeNull();
    expect(parseFloorEvent(frame('people.employee.updated', { job_id: 'j-1' }))).toBeNull();
    expect(parseFloorEvent('not json')).toBeNull();
  });
});

describe('the layout: fixed places', () => {
  test('the declared routes between parts, each once, and a route out names the peer', () => {
    expect(declaredRoutes(tree(), 'engineering')).toEqual([
      { from: 'gates', to: 'loading-dock', target: 'loading-dock' },
      { from: 'loading-dock', to: 'probe-shed', target: 'probe-shed' },
    ]);
    expect(declaredRoutes(tree(OUT_ROWS), 'engineering')).toContainEqual({ from: 'probe-shed', to: null, target: 'operations' });
  });

  test('the fallback grid lays the parts along the routes, the admission machine left, the Outbound record right, the peer beyond it', () => {
    const l = layoutFloor(tree(OUT_ROWS), 'engineering');
    const x = (p: string) => l.parts.get(p)!.x;
    expect(x('gates')).toBe(x('design'));
    expect(x('loading-dock')).toBeGreaterThan(x('gates'));
    expect(x('probe-shed')).toBeGreaterThan(x('loading-dock'));
    expect(l.admission.x + l.admission.w).toBeLessThan(x('gates'));
    expect(l.outbound.x).toBeGreaterThan(x('probe-shed'));
    expect([...l.peers.keys()]).toEqual(['operations']);
    expect(l.peers.get('operations')!.x).toBeGreaterThan(l.outbound.x);
    expect([...l.parts.values()].every((b) => b.placed === 'grid')).toBe(true);
    // The same tree lays out the same floor, every time.
    expect(layoutFloor(tree(OUT_ROWS), 'engineering')).toEqual(l);
  });

  test('a row that declares its placement keeps it, and the rest take the grid below', () => {
    const rows = TREE_ROWS.map((r) => (r.name === 'gates' ? { ...r, placement: { x: 590, y: 120, w: 230, h: 170 } } : r.name === 'forge' ? { ...r, placement: { x: 300, y: 120, w: 230, h: 110 } } : r));
    const l = layoutFloor(tree(rows), 'engineering');
    const g = l.parts.get('gates')!;
    const f = l.parts.get('forge')!;
    expect(g.placed).toBe('declared');
    expect([g.x - f.x, g.y - f.y, g.w, g.h]).toEqual([290, 0, 230, 170]);
    expect(l.parts.get('design')!.placed).toBe('grid');
    expect(l.parts.get('design')!.y).toBeGreaterThanOrEqual(g.y + g.h);
  });

  test('only `placement` places a part: v1’s `floor` cell, or a box under that key, takes the grid', () => {
    // Car A (backlog e3a927c3) serves the box as `placement`; `floor` is
    // the v1 grid cell the station tree retired, and is not read here.
    const env = treeRead(undefined, TREE_ROWS);
    const v1 = (floor: Readonly<Record<string, number>>) => ({
      ...env,
      data: env.data.map((n) => (n.station.name === 'gates' ? { ...n, station: { ...n.station, floor } } : n)),
    });
    const floors: ReadonlyArray<Readonly<Record<string, number>>> = [{ row: 0, col: 3 }, { x: 590, y: 120, w: 230, h: 170 }];
    for (const floor of floors) {
      expect(layoutFloor(parseTree(v1(floor)), 'engineering').parts.get('gates')!.placed).toBe('grid');
    }
    // An empty box is no placement: the grid, never a part drawn at no size.
    const empty = TREE_ROWS.map((r) => (r.name === 'gates' ? { ...r, placement: { x: 10, y: 10, w: 0, h: 40 } } : r));
    expect(layoutFloor(tree(empty), 'engineering').parts.get('gates')!.placed).toBe('grid');
  });

  test('a leaf’s floor is the station itself', () => {
    const l = layoutFloor(tree(), 'gates');
    expect([...l.parts.keys()]).toEqual(['gates']);
  });
});

describe('the floor', () => {
  test('the head as tokens in the station’s order, the rest as the pile, the held packet in a working slot and not in the queue', () => {
    const m = floorModel(tree(), 'engineering', reads(), NOW);
    const gates = m.parts.find((p) => p.name === 'gates')!;
    const waiting = m.tokens.filter((t) => t.place === 'waiting');
    expect(waiting.map((t) => t.key)).toEqual(['j-1', 'j-2', 'j-3', 'j-4']);
    expect(waiting).toHaveLength(HEAD_MAX);
    expect(gates.pile?.bands).toEqual([0, 0, 1, 1]);
    expect(gates.queue).toEqual({ kind: 'ready', total: 7 });
    const held = m.tokens.find((t) => t.key === 'j-held')!;
    expect(held.place).toBe('working');
    expect(held.meta).toBe('■ gate-runner');
    expect(held.label).toBe('Held at the gates');
    // A capacity of two declared: two slots, one filled.
    expect(gates.slots).toHaveLength(2);
    expect([held.x, held.y]).toEqual([gates.slots[0]!.x, gates.slots[0]!.y]);
    expect(gates.flow).toBe('in 3/d · out 2/d');
  });

  test('a token still past its bound is orange; a station holding work still past its bound is too, an empty one never', () => {
    const m = floorModel(tree(), 'engineering', reads(), NOW);
    expect(m.tokens.find((t) => t.key === 'j-2')!.still.past).toBe(true);
    expect(m.tokens.find((t) => t.key === 'j-1')!.still.past).toBe(false);
    const quiet = floorModel(tree(), 'engineering', reads({ claims: { kind: 'ready', data: parsePageClaims(claimsRead([])) }, queues: new Map([['gates', queue('gates', [{ id: 'old', title: 'Old', openedAt: ago(3) }])]]) }), NOW);
    const g = quiet.parts.find((p) => p.name === 'gates')!;
    expect(g.holding).toBe(true);
    expect(g.still.past).toBe(true);
    const probe = quiet.parts.find((p) => p.name === 'probe-shed')!;
    expect(probe.holding).toBe(false);
    expect(probe.queue).toEqual({ kind: 'empty' });
  });

  test('a move the stream saw marks the token and its station moving, and resets the station’s stillness', () => {
    const m = floorModel(tree(), 'engineering', reads({ movedJobs: new Map([['j-1', NOW - 1000]]), movedStations: new Map([['gates', NOW - 1000]]) }), NOW);
    expect(m.tokens.find((t) => t.key === 'j-1')!.moving).toBe(true);
    const gates = m.parts.find((p) => p.name === 'gates')!;
    expect(gates.moving).toBe(true);
    expect(gates.still.words).toBe('just moved');
    expect(m.routes.find((r) => r.key === 'gates>loading-dock')!.hot).toBe(true);
  });

  test('a route is dashed when its source recorded no departure in the window, and not judged when the flow is unread', () => {
    const m = floorModel(tree(), 'engineering', reads(), NOW);
    const route = (k: string) => m.routes.find((r) => r.key === k)!;
    expect(route('gates>loading-dock').dashed).toBe(false);
    expect(route('loading-dock>probe-shed').dashed).toBe(true);
    expect(route('loading-dock>probe-shed').title).toBe('loading-dock recorded no departure in the last day, so nothing crossed here');
    // The admission machine feeds every part no sibling routes into.
    expect(m.routes.filter((r) => r.key.startsWith('admission>')).map((r) => r.key)).toEqual(['admission>design', 'admission>forge', 'admission>gates']);
    const unread = floorModel(tree(), 'engineering', reads({ flow: { kind: 'failed', error: '/api/stations/flow: HTTP 503' } }), NOW);
    expect(unread.routes.every((r) => r.unread && !r.dashed)).toBe(true);
    expect(unread.notes).toContain('The flow read failed, so no route is judged: /api/stations/flow: HTTP 503');
  });

  test('every failed read is said where it would have drawn', () => {
    const m = floorModel(tree(), 'engineering', reads({
      claims: { kind: 'failed', error: '/api/claims: HTTP 503' },
      queues: new Map([['gates', { kind: 'failed', error: '/api/stations/gates/queue: HTTP 500' }]]),
    }), NOW);
    expect(m.tokens).toEqual([]);
    expect(m.parts.find((p) => p.name === 'gates')!.queue).toEqual({ kind: 'unread', why: 'the queue read failed: /api/stations/gates/queue: HTTP 500' });
    expect(m.notes[0]).toBe('The claims read failed, so no working slot is filled: /api/claims: HTTP 503');
  });

  test('a claim the record cannot place is said beside the floor, never placed', () => {
    const m = floorModel(tree(), 'engineering', reads({ claims: { kind: 'ready', data: parsePageClaims(claimsRead([heldClaim('j-x', null)])) } }), NOW);
    expect(m.notes).toContain('1 open claim the record cannot place at any station.');
    expect(m.tokens.some((t) => t.key === 'j-x')).toBe(false);
  });

  test('the queues read: each part the load says holds a packet, and each part with stations of its own', () => {
    const t = tree();
    expect(queuesToRead(t, 'engineering', reads().load)).toEqual(['forge', 'gates']);
    expect(queuesToRead(t, 'engineering', { kind: 'loading' })).toEqual([]);
    expect(queuesToRead(t, 'engineering', { kind: 'failed', error: 'x' })).toEqual(['design', 'forge', 'gates', 'loading-dock', 'probe-shed']);
  });
});
