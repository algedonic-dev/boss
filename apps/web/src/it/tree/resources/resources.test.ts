import { describe, expect, test } from 'bun:test';
import { parseStationLoad, type StationLoadRow } from '../../marshalling/marshalling';
import { ADDRESSES, claimsRead, heldClaim, treeRead } from '../../../../tests/fixtures/station-tree';
import { parseRoute } from '../../../router';
import { parseTree, type StationTree } from '../station-tree';
import {
  GEO, UNPLACED, boxOf, drawnPools, floorGeometry, lineStationOf, parsePools, parseUnitClaims, parseWaitingLine, resourceFloor, resourcesHref,
  seatUnits, unitMoves,
  type FloorReads, type LineRead, type PoolRow, type ResourceFloor, type Seats,
} from './resources';

// THE RESOURCES VIEW (backlog 87575d69, car C of David's station
// visuals, 2026-11-19; prototype artboard Resources). A station's pools as
// a rail of units, each unit drawn where GET /api/claims says it is held;
// equipment drawn inside its owning station and never moving; each
// station's pressure said in words. Every unit reads as lendable until
// car D (30c1efe8) records reservations.

// company ─ engineering ─ build (6 opus, 4 sol)
//         │             ├ review
//         │             └ gates (equipment, 3 seats, 12 vCPU of w-1)
//         └ executive
const ROWS: NonNullable<Parameters<typeof treeRead>[1]> = [
  { name: 'company' },
  { name: 'engineering', parent: 'company', allocation: { 'host:w-1': '28', 'robots:opus': '6', 'robots:sol': '4' } },
  { name: 'build', parent: 'engineering', allocation: { 'robots:opus': '6', 'robots:sol': '4' } },
  { name: 'review', parent: 'engineering' },
  { name: 'gates', parent: 'engineering', machines: ['automation:gate-runner'], capacity: 3, allocation: { 'host:w-1': '12' } },
  { name: 'executive', parent: 'company' },
];
const tree = (): StationTree => parseTree(treeRead('company', ROWS));

/** GET /api/pools as the registry served it on 2026-11-19. */
const POOLS_WIRE = [
  { name: 'budget:robots', version: 2, status: 'active', unit: 'USD/h', kind: 'flow', shape: 'rate', capacity: { state: 'unknown' },
    capacity_from: 'Unknown until a reader derives it: no reader sums them into this row yet.' },
  { name: 'host:w-1', version: 3, status: 'active', unit: 'vCPU', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '32' } },
  { name: 'robots:opus', version: 2, status: 'active', unit: 'slot', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '6' } },
  { name: 'robots:sol', version: 2, status: 'active', unit: 'slot', kind: 'flow', shape: 'capacity', capacity: { state: 'configured', amount: '4' } },
  { name: 'disk:build-cache', version: 3, status: 'active', unit: 'GiB', kind: 'stock', capacity: { state: 'unknown' } },
];
const pools = (): ReadonlyArray<PoolRow> => parsePools(POOLS_WIRE);

/** A builder run's claim, as claims_console.rs serves one: one opus slot
 *  and 2.5 vCPU of w-1, the host draw counted in engineering's share. */
const run = (job: string, station: string | null, over: Readonly<Record<string, unknown>> = {}) =>
  heldClaim(job, station, {
    allocations: [{ pool: 'host:w-1', version: 3, amount: '2.5' }, { pool: 'robots:opus', version: 2, amount: '1' }],
    resource_acquisition: { workflow_kind: 'agent-run', bill: { host: { node: 'w-1', station: 'engineering' } } },
    since: `2026-11-19T17:0${job.length % 10}:00Z`,
    ...over,
  });

const load = (rows: ReadonlyArray<Readonly<{ station: string; depth: number }>>): ReadonlyMap<string, StationLoadRow> =>
  new Map(parseStationLoad({
    data: rows.map((r) => ({ station: r.station, kind: 'group', workflow_kind: null, depth: r.depth, oldest_age_days: 1, also_elsewhere: 0 })),
  }).map((r) => [r.station, r] as const));

const reads = (over: Partial<FloorReads> = {}): FloorReads => ({
  tree: tree(),
  station: 'engineering',
  pools: pools(),
  claims: parseUnitClaims(claimsRead([run('j-a', 'build'), run('j-b', 'build')])),
  seats: new Map(),
  load: { kind: 'ready', data: load([{ station: 'build', depth: 889 }]) },
  lines: new Map(),
  ...over,
});

const floor = (over: Partial<FloorReads> = {}): ResourceFloor => resourceFloor(reads(over));
const box = (f: ResourceFloor, name: string) => {
  const b = f.boxes.find((x) => x.name === name);
  if (b === undefined) throw new Error(`no box ${name}`);
  return b;
};

describe('the address', () => {
  test('a station\'s Resources view is its page\'s address plus /resources, its own route', () => {
    // Below the station's page, which sits below the root's tree floor
    // since backlog 92bfcb04; the /it/tree spelling answers nothing.
    expect(resourcesHref('engineering')).toBe('/tree/engineering/resources');
    expect(resourcesHref('a b')).toBe('/tree/a%20b/resources');
    expect(parseRoute('/tree/engineering/resources', '', ADDRESSES)).toEqual({ kind: 'systemStationResources', station: 'engineering' });
    expect(parseRoute('/tree/a%20b/resources', '', ADDRESSES)).toEqual({ kind: 'systemStationResources', station: 'a b' });
    expect(parseRoute('/tree/engineering', '', ADDRESSES)).toEqual({ kind: 'systemStation', station: 'engineering' });
    expect(parseRoute('/tree/engineering/other', '', ADDRESSES).kind).toBe('notFound');
    expect(parseRoute('/it/tree/engineering/resources', '', ADDRESSES).kind).toBe('notFound');
  });
});

describe('the pools a station allocates', () => {
  test('its own allocation sizes each pool; robots are units, vCPU a bar, the robots\' budget a rate', () => {
    const drawn = drawnPools(tree(), 'engineering', pools());
    expect(drawn.map((p) => [p.name, p.draw])).toEqual([
      ['robots:opus', 'tokens'], ['robots:sol', 'tokens'], ['host:w-1', 'bar'], ['budget:robots', 'rate'],
    ]);
    const opus = drawn.find((p) => p.name === 'robots:opus');
    expect(opus?.draw === 'tokens' ? [opus.size, opus.sizeFrom] : null).toEqual([6, 'allocation']);
    const host = drawn.find((p) => p.name === 'host:w-1');
    expect(host?.draw === 'bar' ? [host.size, host.shares] : null).toEqual([28, [{ station: 'gates', amount: 12 }]]);
  });

  test('the root allocates what its children hold, sized by the registry', () => {
    const drawn = drawnPools(tree(), 'company', pools());
    const opus = drawn.find((p) => p.name === 'robots:opus');
    expect(opus?.draw === 'tokens' ? [opus.size, opus.sizeFrom] : null).toEqual([6, 'registry']);
    const host = drawn.find((p) => p.name === 'host:w-1');
    expect(host?.draw === 'bar' ? host.size : null).toBe(32);
  });

  test('a leaf draws its own allocation and the budget; no host it holds none of', () => {
    expect(drawnPools(tree(), 'build', pools()).map((p) => p.name)).toEqual(['robots:opus', 'robots:sol', 'budget:robots']);
  });

  test('budget:robots states plainly that its rate is unread, in the registry\'s own words', () => {
    const rate = drawnPools(tree(), 'engineering', pools()).find((p) => p.name === 'budget:robots');
    expect(rate?.draw).toBe('rate');
    if (rate?.draw !== 'rate') return;
    expect(rate.rate).toBeNull();
    expect(rate.words).toBe('rate not read: Unknown until a reader derives it: no reader sums them into this row yet.');
  });

  test('a pool no registry row declares is said, never sized', () => {
    const drawn = drawnPools(tree(), 'engineering', pools().filter((p) => p.name !== 'robots:sol'));
    const sol = drawn.find((p) => p.name === 'robots:sol');
    expect(sol).toEqual({ name: 'robots:sol', unit: '', draw: 'unknown', why: 'no pool row in the registry names it' });
  });

  test('a wrong /api/pools shape throws rather than draw no pool', () => {
    expect(() => parsePools({ data: [] })).toThrow();
  });
});

describe('where a held unit stands', () => {
  test('in the child of the station whose subtree holds the claim, or in the station itself', () => {
    const t = tree();
    expect(boxOf(t, 'company', 'build')).toBe('engineering');
    expect(boxOf(t, 'engineering', 'build')).toBe('build');
    expect(boxOf(t, 'engineering', 'engineering')).toBe('engineering');
    expect(boxOf(t, 'build', 'build')).toBe('build');
  });
  test('a claim the record cannot place, or one outside the station, is unplaced', () => {
    expect(boxOf(tree(), 'engineering', null)).toBeNull();
    expect(boxOf(tree(), 'engineering', 'executive')).toBeNull();
    expect(boxOf(tree(), 'engineering', 'no-such-station')).toBeNull();
  });
});

describe('a unit keeps its identity while its claim holds it', () => {
  const h = (key: string, n = 1) => ({ key, n });
  test('a new claim takes the lowest free unit; a held unit never jumps', () => {
    const one = seatUnits(new Map(), [h('a'), h('b')], 6);
    expect([...one.seats]).toEqual([['a', [0]], ['b', [1]]]);
    const two = seatUnits(one.seats, [h('b'), h('c', 2)], 6);
    expect([...two.seats]).toEqual([['b', [1]], ['c', [0, 2]]]);
    expect(two.over).toBe(0);
  });
  test('more held than the pool holds is counted over, never drawn as a unit that does not exist', () => {
    const r = seatUnits(new Map(), [h('a', 3), h('b', 2)], 4);
    expect([...r.seats]).toEqual([['a', [0, 1, 2]], ['b', [3]]]);
    expect(r.over).toBe(1);
  });
});

describe('the floor', () => {
  test('units stand where the claims hold them; the rest wait in the rail as lendable', () => {
    const f = floor();
    const opus = f.tokens.find((t) => t.pool === 'robots:opus');
    expect(opus?.units.map((u) => u.box)).toEqual(['build', 'build', null, null, null, null]);
    expect(opus?.units.map((u) => u.held === null)).toEqual([false, false, true, true, true, true]);
    expect(opus?.free).toBe(4);
    // Car D (30c1efe8) records reservations; until then no unit is reserved.
    expect(opus?.units.every((u) => u.reserved === null)).toBe(true);
    expect(f.tokens.find((t) => t.pool === 'robots:sol')?.free).toBe(4);
  });

  test('the boxes are the station itself and each child, in the tree\'s order, at fixed places', () => {
    expect(floor().boxes.map((b) => [b.name, b.here])).toEqual([
      ['engineering', true], ['build', false], ['gates', false], ['review', false],
    ]);
    // A leaf is one box: itself.
    expect(floor({ station: 'build' }).boxes.map((b) => b.name)).toEqual(['build']);
  });

  test('the vCPU bar sums what the subtree\'s claims hold of it', () => {
    const host = floor().pools.find((p) => p.name === 'host:w-1');
    expect(host?.draw === 'bar' ? host.held : null).toBe(5);
  });

  test('equipment is drawn inside its owning station and never moves', () => {
    const g = box(floor({ claims: parseUnitClaims(claimsRead([heldClaim('j-g', 'gates', { holder: 'automation:gate-runner' })])) }), 'gates');
    expect(g.equipment).toEqual({ count: 3, claimed: 1 });
    expect(box(floor(), 'build').equipment).toBeNull();
  });
});

describe('each station states its pressure', () => {
  test('working: it holds units', () => {
    expect(box(floor(), 'build').pressure).toEqual({ state: 'working', words: 'working: holds 2 units' });
  });

  test('starved: work waits while units sit idle', () => {
    const f = floor({ load: { kind: 'ready', data: load([{ station: 'build', depth: 889 }, { station: 'review', depth: 49 }]) } });
    expect(box(f, 'review').pressure).toEqual({ state: 'starved', words: 'starved: 49 open here while 8 units sit idle' });
  });

  test('waiting: a step of its own stands in the parent\'s claim line', () => {
    const line: LineRead = { kind: 'ready', data: { pool: 'robots:opus', entries: [{ position: 1, jobId: 'j-r', stepId: 's-r', assignee: 'agent-codex', since: '2026-11-19T17:00:00Z', station: 'review' }] } };
    const f = floor({ lines: new Map([['robots:opus', line]]), load: { kind: 'ready', data: load([{ station: 'review', depth: 49 }]) } });
    expect(box(f, 'review').pressure).toEqual({ state: 'waiting', words: 'waiting in engineering\'s line: #1 for robots:opus' });
  });

  test('not asking: work waits, no unit is idle, and none of its steps stands in the line', () => {
    const six = Array.from({ length: 6 }, (_, i) => run(`j-${i}`, 'build', { step_id: `s-${i}` }));
    const sol = Array.from({ length: 4 }, (_, i) => heldClaim(`k-${i}`, 'build', { allocations: [{ pool: 'robots:sol', version: 2, amount: '1' }] }));
    const f = floor({
      claims: parseUnitClaims(claimsRead([...six, ...sol])),
      load: { kind: 'ready', data: load([{ station: 'review', depth: 3 }]) },
    });
    expect(box(f, 'review').pressure).toEqual({ state: 'not-asking', words: 'not asking: 3 open here, no unit idle, no request in the line' });
  });

  test('quiet: nothing open, nothing held', () => {
    expect(box(floor(), 'review').pressure).toEqual({ state: 'quiet', words: 'quiet: nothing open here' });
  });

  test('an unread load is said, never read as quiet', () => {
    expect(box(floor({ load: { kind: 'failed', error: 'HTTP 503' } }), 'review').pressure)
      .toEqual({ state: 'unread', words: 'the load read failed, so whether work waits here is unread: HTTP 503' });
  });
});

describe('the parent\'s claim line', () => {
  test('the line in order, as GET /api/claims/waiting serves it', () => {
    const l = parseWaitingLine({
      pool: 'robots:opus',
      line: [
        { position: 1, job_id: 'j-1', step_id: 's-1', assignee_id: 'agent-codex', waiting_on: { resource: 'robots:opus', reason: 'capacity-exhausted', since: '2026-11-19T17:00:00Z' } },
        { position: 2, job_id: 'j-2', step_id: 's-2', assignee_id: null, waiting_on: { resource: 'robots:opus', reason: 'capacity-exhausted' } },
      ],
    });
    expect(l).toEqual({ pool: 'robots:opus', entries: [
      { position: 1, jobId: 'j-1', stepId: 's-1', assignee: 'agent-codex', since: '2026-11-19T17:00:00Z', station: null },
      { position: 2, jobId: 'j-2', stepId: 's-2', assignee: null, since: null, station: null },
    ] });
    expect(() => parseWaitingLine({ pool: 'robots:opus' })).toThrow();
  });

  test('a waiting step is placed by the station its own metadata declares, and nowhere by guessing', () => {
    const job = { id: 'j-1', steps: [
      { id: 's-1', metadata: { at: 'id-review' } },
      { id: 's-2', metadata: {} },
      { id: 's-3', metadata: { at: 'id-nowhere' } },
    ] };
    expect(lineStationOf(tree(), job, 's-1')).toBe('review');
    expect(lineStationOf(tree(), job, 's-2')).toBeNull();
    expect(lineStationOf(tree(), job, 's-3')).toBeNull();
    expect(lineStationOf(tree(), job, 's-9')).toBeNull();
  });

  test('the floor\'s line is every drawn pool\'s, each pool\'s in its own order, a failed read said', () => {
    const ready: LineRead = { kind: 'ready', data: { pool: 'robots:opus', entries: [{ position: 1, jobId: 'j-1', stepId: 's-1', assignee: null, since: null, station: null }] } };
    const f = floor({ lines: new Map<string, LineRead>([['robots:opus', ready], ['robots:sol', { kind: 'failed', error: 'HTTP 403' }]]) });
    expect(f.line.map((l) => [l.pool, l.state])).toEqual([['robots:opus', 'ready'], ['robots:sol', 'failed']]);
  });
});

describe('fixed places on the floor', () => {
  test('a box stands where it stands whatever is held; an idle unit sits in its rail socket, a held one in its box', () => {
    const busy = floor();
    const idle = floor({ claims: [] });
    const g = floorGeometry(busy, 1000);
    expect(g.boxes).toEqual(floorGeometry(idle, 1000).boxes);
    const build = g.boxes.get('build')!;
    // Unit 1 of opus (held at build) is in build's box, row 0, column 0.
    expect(g.units.get('robots:opus#0')).toEqual({ x: build.x + GEO.pad, y: build.y + GEO.head });
    // Unit 3 is idle: it sits in its socket in the rail.
    expect(g.units.get('robots:opus#2')).toEqual(g.sockets.get('robots:opus#2'));
    // Unit i keeps column i in the rail and in every box.
    expect(g.sockets.get('robots:opus#2')!.x - g.sockets.get('robots:opus#0')!.x).toBe(2 * GEO.pitch);
  });

  test('seats are drawn in their owning station, a unit held where the record cannot place it in its own box', () => {
    const f = floor({ claims: parseUnitClaims(claimsRead([run('j-x', null)])) });
    expect(f.unplaced).toBe(1);
    const g = floorGeometry(f, 1000);
    const gates = g.boxes.get('gates')!;
    expect(g.equipment.get('gates#2')?.x).toBe(gates.x + GEO.pad + 2 * GEO.pitch);
    expect([...g.equipment.keys()].every((k) => k.startsWith('gates#'))).toBe(true);
    expect(g.units.get('robots:opus#0')).toEqual({ x: g.boxes.get(UNPLACED)!.x + GEO.pad, y: g.boxes.get(UNPLACED)!.y + GEO.head });
  });
});

describe('a record of resource moves', () => {
  test('a unit taken and a unit returned are each one line, stamped with the event that moved it', () => {
    const before = floor();
    const seats: Seats = new Map(before.tokens.map((t) => [t.pool, t.seats] as const));
    const after = floor({ seats, claims: parseUnitClaims(claimsRead([run('j-b', 'build'), run('j-c', 'build', { holder: 'agent-claude-b' })])) });
    const moves = unitMoves(before, after, '2026-11-19T17:30:00Z');
    expect(moves.map((m) => m.text)).toEqual([
      'build returns robots:opus unit 1 to engineering (agent-claude, agent-run)',
      'build takes robots:opus unit 1 from engineering (agent-claude-b, agent-run)',
    ]);
    expect(moves.every((m) => m.at === '2026-11-19T17:30:00Z')).toBe(true);
  });

  test('nothing moved is no line', () => {
    expect(unitMoves(floor(), floor({ seats: new Map(floor().tokens.map((t) => [t.pool, t.seats] as const)) }), 'x')).toEqual([]);
  });
});
