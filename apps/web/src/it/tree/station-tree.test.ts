import { describe, expect, test } from 'bun:test';
import { parseStationLoad } from '../marshalling/marshalling';
import { ADDRESSES, CLAIMS, POOLS, claimsRead, idOf, stationList, stationLoad, treeRead } from '../../../tests/fixtures/station-tree';
import { parseRoute } from '../../router';
import { ROUTE_CATALOG, STATION } from '../../shell/nav-catalog';
import { pagePath } from '../../shell/page-address';
import {
  DEFAULT_DEPTH, claimsPath, depthOf, parseClaims, parseTree, partWords, poolFigure, rootIdOf, stationCard, treeHref,
  type CardPool, type CardReads, type StationCardModel,
} from './station-tree';

// THE TREE READ, PARSED — and what one card says about one station
// (backlog 94a9dad4, T7 of design 4b27a65a; the card itself is design
// c4854fd4 E1, backlog e1b13a15). Pure: the fixtures stand in for GET
// /api/stations?under=, /api/stations/load and /api/claims exactly as
// the server serves them.

const tree = () => parseTree(treeRead());
const loadMap = () => new Map(parseStationLoad(stationLoad()).map((r) => [r.station, r] as const));
/** The three reads as the floor holds them once each has answered. */
const reads = (over: Partial<CardReads> = {}): CardReads => ({
  load: { kind: 'ready', data: loadMap() },
  claims: { kind: 'ready', data: parseClaims(claimsRead()) },
  ...over,
});
const card = (name: string, r: CardReads = reads(), t = tree()): StationCardModel => {
  const m = stationCard(t, name, r);
  if (m === null) throw new Error(`no card for ${name}`);
  return m;
};
const pool = (m: StationCardModel, name: string): CardPool | undefined =>
  m.pools.flatMap((g) => g.pools).find((p) => p.pool === name);
/** A claim as the server serves it, at a station, holding allocations. */
const claim = (station: string | null, allocations: ReadonlyArray<Readonly<{ pool: string; amount: string }>> = []) => ({
  step_id: `s-${station}`, job_id: 'j', holder: 'agent-claude', executor_kind: 'robot', station,
  station_unavailable: station === null ? 'history-not-rebuilt' : null,
  allocations: allocations.map((a) => ({ ...a, version: 1 })),
});

describe('the tree read', () => {
  test('parses the subtree root first, with every station, address and child', () => {
    const t = tree();
    expect(t.top).toBe('company');
    // Sixteen, plus the fixture's three page hosts (design, vault, finance; backlog 0bc2eb68).
    expect(t.nodes).toHaveLength(19);
    expect(t.nodes[0]?.name).toBe('company');
    const gates = t.byName.get('gates');
    expect(gates?.address).toBe('/company/engineering/gates');
    expect(gates?.capacity).toBe(2);
    expect(gates?.machines).toEqual(['automation:gate-runner']);
    expect(gates?.allocation).toEqual({ 'host:w-1': 8 });
    expect(gates?.edges).toEqual(['loading-dock']);
    expect(t.byName.get('engineering')?.children).toEqual(['design', 'forge', 'gates', 'loading-dock', 'probe-shed']);
    expect(t.pools.map((p) => p.name)).toEqual(POOLS.map((p) => p.pool.name));
  });

  test('names every gap the server named, and fills none', () => {
    const t = tree();
    expect(t.pools.find((p) => p.name === 'host:w-1')?.host).toBe('w-1 · the build node');
    expect(t.pools.find((p) => p.name === 'host:w-2')?.hostGap).toBe('no estate node `w-2` is declared');
    expect(t.pools.find((p) => p.name === 'disk:ci-workspaces')?.capacity).toBeNull();
    expect(t.gaps.some((g) => g.includes('forge') && g.includes('no host pool'))).toBe(true);
    expect(t.gaps.some((g) => g.includes('my-watchlist') && g.includes('outside the tree'))).toBe(true);
  });

  test('a partial read is said, not refused: no pools is a gap, not a crash', () => {
    const raw = { ...treeRead(), pools: [], pools_unavailable: 'the pool registry could not be read: down', hosts_unavailable: 'this caller may not read the estate registry' };
    const t = parseTree(raw);
    expect(t.pools).toEqual([]);
    expect(t.gaps).toContain('the pool registry could not be read: down');
    expect(t.gaps).toContain('this caller may not read the estate registry');
  });

  test('a wrong shape is a throw, never an empty floor', () => {
    expect(() => parseTree({ under: 'x' })).toThrow('station tree');
    expect(() => parseTree({ ...treeRead(), data: [] })).toThrow('no station');
    expect(() => parseTree({ ...treeRead(), data: [{ address: '/x', depth: 0, children: [] }] })).toThrow('station tree');
    expect(() => parseTree(null)).toThrow('station tree');
  });

  test('a server that predates the tree read ignores `under` and answers the list: refused, not drawn', () => {
    // A rollback onto a build before T2+T5 (backlog 3f58c823) serves
    // GET /api/stations?under=x as the plain list of station specs.
    expect(() => parseTree(stationList())).toThrow('station tree');
  });

  test('the root is found by the tree shape, from the station list', () => {
    expect(rootIdOf(stationList())).toBe(idOf('company'));
    const two = { data: [...stationList().data, { name: 'rival', id: 'id-rival', status: 'active' }] };
    expect(rootIdOf(two)).toBeNull();
    expect(rootIdOf({ data: stationList().data.filter((s) => !('id' in s) || s.name !== 'company') })).toBeNull();
    expect(rootIdOf({ data: 'nope' })).toBeNull();
  });
});

// THE STATION CARD (design c4854fd4 E1, decided by David 2026-11-07
// 23:38Z; backlog e1b13a15). Five rows on every station kind — rail,
// band, pools, parts, and nothing else — drawn from REAL, CURRENT reads
// only: the station rows, the open-packet load, the open claims. A
// figure a read does not carry is said to be unrecorded, never drawn as
// a zero.
describe('the claims read', () => {
  test('reads each claim for its station and allocations, and the projection for an unavailable source', () => {
    const c = parseClaims(claimsRead([claim('gates', [{ pool: 'host:w-1', amount: '4' }]), claim(null)]));
    expect(c.claims).toEqual([
      { station: 'gates', allocations: [{ pool: 'host:w-1', amount: 4, station: 'gates' }] },
      { station: null, allocations: [] },
    ]);
    expect(c.sourceUnavailable).toBe(false);
    expect(parseClaims(claimsRead(CLAIMS, '2026-11-08T03:00:00Z')).sourceUnavailable).toBe(true);
  });

  test('a read narrowed to a station says which; a whole read says none (backlog d05e29a7)', () => {
    expect(parseClaims(claimsRead()).within).toBeNull();
    expect(parseClaims({ ...claimsRead(), within: 'engineering' }).within).toBe('engineering');
  });

  test('a wrong shape is a throw, never an empty floor of free cells', () => {
    expect(() => parseClaims([])).toThrow('claims read');
    expect(() => parseClaims({ data: [] })).toThrow('claims read');
    expect(() => parseClaims(null)).toThrow('claims read');
  });
});

describe('the station card: five rows on every kind', () => {
  test('the card carries the five rows and nothing else: no packets, no moves, no connections', () => {
    for (const name of ['company', 'engineering', 'gates', 'executive', 'forge-ci']) {
      expect(Object.keys(card(name)).sort()).toEqual(['lines', 'mode', 'name', 'parts', 'plate', 'pools', 'poolsNote', 'rail']);
    }
  });

  test('every card keeps the hosts, people and robots rows; an empty one says none held so tiles align', () => {
    const exec = card('executive');
    expect(exec.pools.map((g) => g.group)).toEqual(['hosts', 'people', 'robots']);
    expect(exec.pools.every((g) => g.pools.length === 0)).toBe(true);
    const gates = card('gates');
    expect(gates.pools.map((g) => g.group)).toEqual(['hosts', 'people', 'robots']);
    expect(gates.pools.map((g) => g.pools.map((p) => p.pool))).toEqual([['host:w-1'], [], []]);
  });

  test('the root holds every pool whole, in the fixed order: hosts, people, robots, then stock', () => {
    const root = card('company');
    expect(root.pools.map((g) => g.group)).toEqual(['hosts', 'people', 'robots', 'stock']);
    expect(root.pools.map((g) => g.pools.map((p) => p.pool))).toEqual([
      ['host:w-1', 'host:w-2'], ['people:platform-admin'], ['robots:opus'], ['disk:ci-workspaces'],
    ]);
    expect(pool(root, 'host:w-1')).toMatchObject({ held: 32, unit: 'vCPU', draw: 'cells' });
    // A capacity the registry calls unknown is unknown — no cells, never 0.
    expect(pool(root, 'disk:ci-workspaces')).toMatchObject({ held: null, draw: 'none' });
    expect(POOLS.length).toBe(5);
  });

  test('a station holds what its row allocates: one cell per unit, a bar past 32 units', () => {
    expect(pool(card('engineering'), 'host:w-1')).toMatchObject({ held: 24, draw: 'cells' });
    expect(pool(card('gates'), 'host:w-1')).toMatchObject({ held: 8, draw: 'cells' });
    const raw = treeRead();
    const big = {
      ...raw,
      data: raw.data.map((n) => (n.station.name === 'gates' ? { ...n, station: { ...n.station, allocation: { 'host:w-1': '40' } } } : n)),
    };
    expect(pool(card('gates', reads(), parseTree(big)), 'host:w-1')).toMatchObject({ held: 40, draw: 'bar' });
  });
});

describe('the station card: in use is what the claims record, or it says it is not recorded', () => {
  test('no claim carries an allocation (the record as of 2026-11-08): every pool says in use not recorded', () => {
    for (const [name, p] of [['company', 'host:w-1'], ['engineering', 'host:w-1'], ['gates', 'host:w-1']] as const) {
      expect(pool(card(name), p)).toMatchObject({ inUse: null, inUseNote: 'in use not recorded' });
      expect(card(name).poolsNote).toBe('in use not recorded');
    }
    // A card holding no pool has nothing to label.
    expect(card('executive').poolsNote).toBeNull();
  });

  test('recorded allocations sum over the station and the stations inside it; the root counts every claim', () => {
    const recorded = reads({
      claims: { kind: 'ready', data: parseClaims(claimsRead([
        claim('gates', [{ pool: 'host:w-1', amount: '4' }]),
        claim('forge-ci', [{ pool: 'host:w-1', amount: '2' }, { pool: 'robots:opus', amount: '1' }]),
        claim('a.outside.the.tree', [{ pool: 'host:w-1', amount: '1' }]),
      ])) },
    });
    expect(pool(card('gates', recorded), 'host:w-1')).toMatchObject({ inUse: 4, inUseNote: null });
    expect(pool(card('engineering', recorded), 'host:w-1')).toMatchObject({ inUse: 6, inUseNote: null });
    expect(pool(card('company', recorded), 'host:w-1')).toMatchObject({ inUse: 7, inUseNote: null });
    expect(pool(card('company', recorded), 'robots:opus')).toMatchObject({ inUse: 1 });
    // A pool no claim records anything against is still unrecorded, not 0.
    expect(pool(card('company', recorded), 'host:w-2')).toMatchObject({ inUse: null, inUseNote: 'in use not recorded' });
    expect(card('company', recorded).poolsNote).toBe('in use not recorded: host:w-2, people:platform-admin');
    expect(card('gates', recorded).poolsNote).toBeNull();
  });

  // Codex's landed review of car 1 (backlog f23ce786, 2026-11-08): the
  // card flattened allocations before judging, so a claim recording none
  // vanished beside one that recorded 3 and the card drew "5 free of 8"
  // that nothing measured. Every claim counted against the pool must
  // record an allocation before the figure is definite.
  test('one claim recording no allocation beside one that records 3 keeps in use unknown, and the note counts both', () => {
    const mixed = reads({
      claims: { kind: 'ready', data: parseClaims(claimsRead([
        claim('gates', [{ pool: 'host:w-1', amount: '3' }]),
        claim('gates'),
      ])) },
    });
    const w1 = pool(card('gates', mixed), 'host:w-1');
    expect(w1).toMatchObject({ held: 8, inUse: null, inUseNote: 'in use not recorded', inUseCounts: '1 recorded, 1 unrecorded' });
    expect(poolFigure(w1!)).toBe('8 vCPU held');
    expect(card('gates', mixed).poolsNote).toBe('in use not recorded (1 recorded, 1 unrecorded)');
    expect(pool(card('company', mixed), 'host:w-1')).toMatchObject({ inUse: null, inUseCounts: '1 recorded, 1 unrecorded' });

    // The control: every counted claim records, so the figure is definite —
    // and a claim recording none OUTSIDE the station is not counted here.
    const known = reads({
      claims: { kind: 'ready', data: parseClaims(claimsRead([
        claim('gates', [{ pool: 'host:w-1', amount: '3' }]),
        claim('design-review'),
      ])) },
    });
    const k = pool(card('gates', known), 'host:w-1');
    expect(k).toMatchObject({ inUse: 3, inUseNote: null, inUseCounts: null });
    expect(poolFigure(k!)).toBe('5 free of 8 vCPU');
    expect(card('gates', known).poolsNote).toBeNull();
  });

  test('an allocation on a claim that names no station cannot be placed, so the pool is not recorded anywhere below the root', () => {
    const unplaced = reads({
      claims: { kind: 'ready', data: parseClaims(claimsRead([
        claim('gates', [{ pool: 'host:w-1', amount: '4' }]),
        claim(null, [{ pool: 'host:w-1', amount: '2' }]),
      ])) },
    });
    expect(pool(card('gates', unplaced), 'host:w-1')).toMatchObject({ inUse: null, inUseNote: 'in use not recorded' });
    expect(pool(card('company', unplaced), 'host:w-1')).toMatchObject({ inUse: 6 });
  });

  test('a failed claims read draws no in-use figure and the decided red rail; a stale one the hollow rail and the STALE plate', () => {
    const failed = card('gates', reads({ claims: { kind: 'failed', error: '/api/claims: HTTP 503' } }));
    expect(failed.rail.mark).toBe('failed');
    expect(pool(failed, 'host:w-1')).toMatchObject({ held: 8, inUse: null, inUseNote: 'in use unread' });
    expect(failed.plate).toMatchObject({ state: 'failed', word: 'read failed' });
    expect(failed.plate.why).toContain('/api/claims: HTTP 503');

    const stale = card('gates', reads({ claims: { kind: 'ready', data: parseClaims(claimsRead(CLAIMS, '2026-11-08T03:00:00Z')) } }));
    expect(stale.rail.mark).toBe('hollow');
    expect(stale.plate).toMatchObject({ state: 'stale', word: 'stale' });
    expect(pool(stale, 'host:w-1')).toMatchObject({ inUse: null, inUseNote: 'in use stale' });
  });
});

describe('the station card: rail, plate and parts', () => {
  test('a retired department label cannot override the parent line', () => {
    const raw = treeRead();
    const data = raw.data.map((row) => row.station.name === 'repair'
      ? { ...row, station: { ...row.station, department: 'it' } }
      : row);
    const parsed = parseTree({ ...raw, data });
    expect(card('repair', reads(), parsed).rail.line).toBe('hosting');
    expect(card('gates', reads(), parsed).rail.line).toBe('it');
  });

  test("the rail is the department's line, inherited by every station inside it; no line is night ink", () => {
    expect(card('engineering').rail).toEqual({ line: 'it', mark: 'solid' });
    expect(card('forge-ci').rail.line).toBe('it');
    expect(card('gates').rail.line).toBe('it');
    expect(card('repair').rail.line).toBe('hosting');
    expect(card('executive').rail.line).toBeNull();
    expect(card('company').rail.line).toBeNull();
  });

  test('one plate, always with its word: clear or unjudged with no load row', () => {
    expect(card('gates').plate).toEqual({ state: 'clear', word: 'clear', why: null });
    expect(card('probe-shed').plate).toEqual({ state: 'unjudged', word: 'unjudged', why: null });
    expect(card('gates', reads({ load: { kind: 'loading' } })).plate).toMatchObject({ state: 'reading', word: 'reading' });
    const noLoad = card('gates', reads({ load: { kind: 'failed', error: '/api/stations/load: HTTP 500' } }));
    expect(noLoad.plate).toMatchObject({ state: 'failed', word: 'read failed' });
    expect(noLoad.rail.mark).toBe('failed');
    expect(noLoad.parts.standing).toEqual({ value: null, note: 'unread' });
    // A refusal is worded as one (7abb184f): the plate says refused and
    // the why keeps the server's reason; the rail is red all the same.
    const why = '/api/stations/load: HTTP 403: emp-x (role intern) may read no packets';
    const refused = card('gates', reads({ load: { kind: 'failed', error: why, refused: true } }));
    expect(refused.plate).toEqual({ state: 'failed', word: 'read refused', why });
    expect(refused.rail.mark).toBe('failed');
    // One refusal beside one outage is still a failure: the word claims
    // no more than every failed read shares.
    const both = card('gates', reads({ load: { kind: 'failed', error: why, refused: true }, claims: { kind: 'failed', error: '/api/claims: HTTP 503' } }));
    expect(both.plate).toMatchObject({ state: 'failed', word: 'read failed' });
  });

  test('parts: sub-stations, ad hoc open now, seats busy of bays, machines, standing packets', () => {
    expect(card('engineering').parts).toEqual({
      subStations: 5, adHocOpen: 0, seats: { bays: null, busy: null, note: null }, machines: 0, standing: { value: null, note: 'no load row' },
    });
    // A claim in the read names no station, so how many of the bays are
    // busy is not recorded — never guessed from the stations that are.
    expect(card('gates').parts).toEqual({
      subStations: 0, adHocOpen: 0, seats: { bays: 2, busy: null, note: 'busy not recorded' }, machines: 1, standing: { value: 3, note: null },
    });
    const placed = reads({ claims: { kind: 'ready', data: parseClaims(claimsRead([claim('gates'), claim('engineering')])) } });
    expect(card('gates', placed).parts.seats).toEqual({ bays: 2, busy: 1, note: null });
    expect(card('company').parts.machines).toBe(1);
  });

  test('an ad hoc station is framed dashed and counted apart from the standing sub-stations', () => {
    const raw = treeRead();
    const eng = raw.data.find((n) => n.station.name === 'engineering');
    const adhoc = {
      station: { name: 'build/a-seat', version: 1, status: 'active', title: 'a workspace', kind: 'group', discipline: ['age'], id: idOf('build/a-seat'), parent: idOf('engineering'), mode: 'ad-hoc', allocation: { 'host:w-1': '4' }, predicate: {}, created_at: '2026-11-08T00:00:00Z' },
      address: '/company/engineering/build/a-seat', depth: 2, children: [],
    };
    const withAdhoc = parseTree({
      ...raw,
      data: [...raw.data.map((n) => (n === eng ? { ...n, children: [...n.children, 'build/a-seat'] } : n)), adhoc],
    });
    expect(card('engineering', reads(), withAdhoc).parts).toMatchObject({ subStations: 5, adHocOpen: 1 });
    const a = card('build/a-seat', reads(), withAdhoc);
    expect(a.mode).toBe('ad-hoc');
    expect(a.rail.line).toBe('it');
    expect(pool(a, 'host:w-1')).toMatchObject({ held: 4, draw: 'cells' });
  });

  test('the card is as tall as its rows: the layout counts the same lines the card draws', () => {
    // pools row: a half line above and below, two lines per drawn pool,
    // one per undrawn pool and per empty group, one for its note; parts
    // row: a half line above and below, and a line per part.
    expect(card('executive').lines).toBe(1 + 3 + 1 + 5);
    expect(card('gates').lines).toBe(1 + (2 + 1 + 1) + 1 + 1 + 5);
    // hosts: w-1 and w-2 (both drawn); people: 1 drawn; robots: 1 drawn; stock: 1 undrawn; the note.
    expect(card('company').lines).toBe(1 + (2 + 2 + 2 + 2 + 1) + 1 + 1 + 5);
  });

  test('the words: free of held when in use is recorded, held when it is not, and every part labelled', () => {
    expect(poolFigure(pool(card('gates'), 'host:w-1')!)).toBe('8 vCPU held');
    expect(poolFigure(pool(card('company'), 'disk:ci-workspaces')!)).toBe('capacity unknown');
    const recorded = reads({ claims: { kind: 'ready', data: parseClaims(claimsRead([claim('gates', [{ pool: 'host:w-1', amount: '3' }])])) } });
    expect(poolFigure(pool(card('gates', recorded), 'host:w-1')!)).toBe('5 free of 8 vCPU');
    expect(partWords(card('gates').parts).map((p) => `${p.label}: ${p.value}`)).toEqual([
      'Sub-stations: 0', 'Ad hoc open: 0', 'Seats busy: busy not recorded · 2 bays', 'Machines: 1', 'Standing: 3',
    ]);
    expect(partWords(card('executive').parts).map((p) => p.value)).toEqual(['2', '0', 'none held', 'none', 'no load row']);
    expect(partWords(card('loading-dock').parts).find((p) => p.key === 'seats')?.value).toBe('busy not recorded · 1 bay');
  });

  test("a host the estate cannot answer for is said beside the floor, not on the card", () => {
    expect(tree().gaps).toContain('host:w-2: no estate node `w-2` is declared');
  });

  test('a station the read does not hold has no card', () => {
    expect(stationCard(tree(), 'nowhere', reads())).toBeNull();
  });
});

describe("the floor's address", () => {
  // The floor is the company root's page since backlog 92bfcb04 (car 3 of
  // design df0ecd8b: "tree — the company root — the whole tree is the
  // company's view of itself"), so its address is the catalog's, and
  // /it/tree answers nothing.
  test('the plain link is plain; a station and a depth ride the query', () => {
    expect(treeHref(null)).toBe('/tree');
    expect(treeHref(null, DEFAULT_DEPTH)).toBe('/tree');
    expect(treeHref('id-engineering', 1)).toBe('/tree?under=id-engineering&depth=1');
    expect(treeHref('a b', 2)).toBe('/tree?under=a+b&depth=2');
  });

  test('the floor is the page the root hosts, at the address the tree gives it', () => {
    expect<string | undefined>(ROUTE_CATALOG['system-tree'].host).toBe(STATION.algedonic);
    expect(pagePath(ROUTE_CATALOG['system-tree'], ADDRESSES)).toBe(treeHref(null));
    expect(parseRoute('/it/tree', '', ADDRESSES)).toEqual({ kind: 'notFound', path: '/it/tree' });
  });

  test('a depth is a whole number from 1 to 9, anything else the default', () => {
    expect(depthOf('1')).toBe(1);
    expect(depthOf('9')).toBe(9);
    for (const bad of [null, '', '0', '10', '-1', '2.5', 'x']) expect(depthOf(bad)).toBe(DEFAULT_DEPTH);
  });

  test('the router reaches the floor with its station and depth', () => {
    expect(parseRoute('/tree', '', ADDRESSES)).toEqual({ kind: 'systemTree' });
    expect(parseRoute('/tree', '?under=id-engineering&depth=2', ADDRESSES)).toEqual({ kind: 'systemTree', under: 'id-engineering', depth: 2 });
    expect(parseRoute('/tree', '?depth=nope', ADDRESSES)).toEqual({ kind: 'systemTree' });
  });
});

// REGION_AT and the four region views it fed retired with the transit map
// (R3, backlog 23a4b4d7): a query that names a view is no view.
describe('no region view rides the floor', () => {
  test('a `view` in the query is ignored by the router', () => {
    expect(parseRoute('/tree', '?view=marshalling', ADDRESSES)).toEqual({ kind: 'systemTree' });
    expect(parseRoute('/tree', '?view=gates&depth=2', ADDRESSES)).toEqual({ kind: 'systemTree', depth: 2 });
  });
});

// Backlog d05e29a7: the floor and the station page each read the claims
// of their own subtree, never every claim on every load.
describe('the claims read names its station', () => {
  test('a station is asked by id or name, encoded; none asks for every claim', () => {
    expect(claimsPath('engineering')).toBe('/api/claims?station=engineering');
    expect(claimsPath('a b/c')).toBe('/api/claims?station=a%20b%2Fc');
    expect(claimsPath(null)).toBe('/api/claims');
  });
});
