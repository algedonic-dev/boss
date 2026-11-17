import { describe, expect, test } from 'bun:test';
import { parseStationLoad } from '../marshalling/marshalling';
import { claimsRead, heldClaim, treeRead } from '../../../tests/fixtures/station-tree';
import { parseRoute } from '../../router';
import { parseTree, type StationTree } from './station-tree';
import {
  COLUMN_MAX, HOLDER_MARK, breadcrumb, constraintBand, parseBinding, parseHeldClaims, parsePageClaims, poolsCaveat, stationHref, stationPage, upstreamOf,
  type PageReads, type PoolBand, type StationPageModel,
} from './station-page';

// THE STATION PAGE (design c4854fd4 E2, decided by David 2026-11-07
// 23:38Z; backlog 72d58e00; re-cut by design 08d1530e, backlog
// 950ad972). Opening a station shows the pools it holds, segmented by
// holder; its parts as cards; Working. REAL, CURRENT reads only (David
// 2026-11-08 00:50Z): the tree, the load and the open claims — a figure
// no read states is said, never computed.

const tree = (): StationTree => parseTree(treeRead());
const loadMap = () => new Map(parseStationLoad({
  data: [
    { station: 'gates', kind: 'group', workflow_kind: 'gate-run', depth: 3, oldest_age_days: 0.4, also_elsewhere: 0 },
    { station: 'engineering', kind: 'group', workflow_kind: null, depth: 0, oldest_age_days: null, also_elsewhere: 0 },
    { station: 'loading-dock', kind: 'group', workflow_kind: 'ship-a-change', depth: 30, oldest_age_days: 2.5, also_elsewhere: 0 },
  ],
  distinct_packets: 33,
}).map((r) => [r.station, r] as const));

const HELD = [
  heldClaim('j-gate-1', 'gates', { holder: 'automation:gate-runner', executor_kind: 'automaton' }),
  heldClaim('j-build-1', 'engineering', { resource_acquisition: { workflow_kind: 'agent-run' } }),
  heldClaim('j-ci-1', 'forge-ci', { holder: 'automation:forge-actions-runner', executor_kind: 'automaton' }),
  heldClaim('j-david', 'design-review', { holder: 'emp-david', executor_kind: 'person' }),
  heldClaim('j-old', null, { holder: 'automation:train-conductor', executor_kind: 'automaton' }),
];

const reads = (over: Partial<PageReads> = {}): PageReads => ({
  load: { kind: 'ready', data: loadMap() },
  claims: { kind: 'ready', data: parsePageClaims(claimsRead(HELD)) },
  ...over,
});

const page = (name: string, r: PageReads = reads()): StationPageModel => {
  const m = stationPage(tree(), name, r);
  if (m === null) throw new Error(`no page for ${name}`);
  return m;
};
const band = (bands: ReadonlyArray<PoolBand>, pool: string): PoolBand => {
  const b = bands.find((x) => x.pool === pool);
  if (b === undefined) throw new Error(`no band for ${pool}`);
  return b;
};
const segs = (b: PoolBand) => b.segments.map((s) => [s.label, s.amount, s.inUse, s.free, s.hatched]);
/** Claims recording allocations, the way a claim will once 35e9babf lands. */
const allocated = (...cs: ReadonlyArray<readonly [string, string | null, string, string]>) =>
  parsePageClaims(claimsRead(cs.map(([job, station, pool, amount]) =>
    heldClaim(job, station, { allocations: [{ pool, version: 1, amount }] }))));

describe('the page address', () => {
  test('a station opens at /it/tree/<name>; the floor keeps /it/tree; a view in the query is ignored', () => {
    expect(stationHref('engineering')).toBe('/it/tree/engineering');
    expect(stationHref('q.platform-admin.task')).toBe('/it/tree/q.platform-admin.task');
    expect(parseRoute('/it/tree/engineering', '')).toEqual({ kind: 'systemStation', station: 'engineering' });
    expect(parseRoute('/it/tree/gates', '?view=track')).toEqual({ kind: 'systemStation', station: 'gates' });
    expect(parseRoute('/it/tree/gates', '?view=nowhere')).toEqual({ kind: 'systemStation', station: 'gates' });
    expect(parseRoute('/it/tree/a%20b', '')).toEqual({ kind: 'systemStation', station: 'a b' });
    expect(parseRoute('/it/tree', '')).toEqual({ kind: 'systemTree' });
    expect(parseRoute('/it/tree/a/b', '').kind).toBe('notFound');
  });
});

describe('the reads the page adds', () => {
  test('a held claim is read for its holder, its kind of executor, its station and since when', () => {
    const held = parseHeldClaims(claimsRead(HELD));
    expect(held[0]).toEqual({
      stepId: 's-j-gate-1', jobId: 'j-gate-1', holder: 'automation:gate-runner', holderKind: 'automaton', station: 'gates',
      since: '2026-11-08T07:00:00Z', workflowKind: null, allocations: [],
    });
    expect(held[1]?.workflowKind).toBe('agent-run');
    expect(held[3]?.holderKind).toBe('person');
    // A kind the taxonomy does not hold is not guessed into one.
    expect(parseHeldClaims(claimsRead([heldClaim('j', 'gates', { executor_kind: 'wizard' })]))[0]?.holderKind).toBeNull();
    expect(() => parseHeldClaims({ data: [] })).toThrow('claims read');
  });

  test('the binding is the server’s: a pool with its reading, none, or unread from a server without the key', () => {
    expect(parseBinding({ claims: [] })).toEqual({ kind: 'unread' });
    expect(parseBinding({ claims: [], binding: null, window_hours: 24 })).toEqual({ kind: 'none', windowHours: 24 });
    expect(parseBinding({
      claims: [], binding: 'host:w-1', window_hours: 24,
      pools: [{ pool_key: 'host:w-1', pool: { unit: 'vCPU', capacity: { state: 'configured', amount: '32' } }, reserved: '7.5', failed_reservations: 1 }],
    })).toEqual({ kind: 'pool', pool: 'host:w-1', windowHours: 24, reading: { unit: 'vCPU', reserved: 7.5, capacity: 32, refused: 1 } });
  });
});

describe('the breadcrumb is the parent chain, and trouble climbs it', () => {
  test('root first, each a page link, the current station last', () => {
    const crumbs = breadcrumb(tree(), 'gates', { load: { kind: 'ready', data: loadMap() }, claims: { kind: 'loading' } });
    expect(crumbs.map((c) => c.name)).toEqual(['company', 'engineering', 'gates']);
    expect(crumbs.map((c) => c.href)).toEqual(['/it/tree/company', '/it/tree/engineering', '/it/tree/gates']);
    expect(crumbs.map((c) => c.current)).toEqual([false, false, true]);
  });

});

// THE WALK UPSTREAM IS THE PARENT (backlog a1c0a0ff, design deba3000).
// Seven station rows carried a hand-typed `upstream.href = "/system/yard"`
// a router that retired /system on 2026-08-31 answers notFound. A page's
// URL is its host station's address, so the station a station's work is
// diagnosed one step above is its parent in the tree, and its link is the
// parent's page: derived from the read, never typed into a row.
describe('the walk upstream is the parent station page, derived from the tree', () => {
  test('a station walks up to its parent, at a page the router serves', () => {
    const up = upstreamOf(tree(), 'gates');
    expect(up).toEqual({ name: 'engineering', title: expect.any(String), href: '/it/tree/engineering' });
    expect(parseRoute(up!.href, '').kind).toBe('systemStation');
  });

  test('the root has no parent, so no walk upstream rather than a guessed one', () => {
    expect(upstreamOf(tree(), 'company')).toBeNull();
  });

  test('the page model carries it', () => {
    const model = stationPage(tree(), 'gates', reads());
    expect(model?.upstream?.href).toBe('/it/tree/engineering');
  });
});

describe('the constraint band: the pools held, segmented by holder', () => {
  test('a department: each part its share, its own floating rest, and the root share that is not its own hatched', () => {
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'ready', data: loadMap() }, claims: { kind: 'ready', data: parsePageClaims(claimsRead(HELD)).card } }), 'host:w-1');
    expect(b.capacity).toBe(32);
    expect(segs(b)).toEqual([
      ['gates', 8, null, null, false],
      ['floating', 16, null, null, false],
      ["not engineering's", 8, null, null, true],
    ]);
    // No claim records an allocation (backlog 35e9babf): in use is said, not zeroed.
    expect(b.inUseNote).toBe('in use not recorded');
  });

  // Design 08d1530e item 7 (backlog 950ad972): "room not stated", the
  // keeper and the row version were said on EVERY pool; they went, and
  // the strip's caveat is said once (`poolsCaveat`). A note left on a
  // band is an anomaly in that pool, like a host the estate cannot read.
  test('a band carries no per-pool caveat; only an anomaly in that pool', () => {
    const bands = constraintBand(tree(), 'company', { load: { kind: 'loading' }, claims: { kind: 'loading' } });
    const b = band(bands, 'host:w-1');
    expect(b.notes).toEqual([]);
    expect(b.host).toBe('w-1 · the build node');
    expect(JSON.stringify(b)).not.toContain('room for');
    expect(band(bands, 'host:w-2').notes).toEqual(['no estate node `w-2` is declared']);
  });

  test('the strip says its caveat once, naming the pools only when some are and some are not', () => {
    const r = { load: { kind: 'loading' } as const, claims: { kind: 'ready' as const, data: parsePageClaims(claimsRead(HELD)).card } };
    const bands = constraintBand(tree(), 'engineering', r);
    expect(poolsCaveat(bands, 'engineering', r)).toBe('In use is not recorded for these pools yet, so the strips show shares, not use.');
    const known = allocated(['a', 'gates', 'host:w-1', '3']);
    const k = { load: { kind: 'loading' } as const, claims: { kind: 'ready' as const, data: { ...known.card, within: 'engineering' } } };
    expect(poolsCaveat(constraintBand(tree(), 'engineering', k), 'engineering', k)).toBe('Use outside engineering is not read on this page.');
    const failed = { load: { kind: 'loading' } as const, claims: { kind: 'failed' as const, error: 'HTTP 503' } };
    expect(poolsCaveat(constraintBand(tree(), 'engineering', failed), 'engineering', failed)).toBe('In use is unread: the claims read failed.');
  });

  test('allocations the claims record fill each segment, and free is what each holds less what it uses', () => {
    const claims = allocated(['a', 'gates', 'host:w-1', '4'], ['b', 'engineering', 'host:w-1', '2'], ['c', 'repair', 'host:w-1', '1']);
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: claims.card } }), 'host:w-1');
    expect(b.inUseNote).toBeNull();
    expect(segs(b)).toEqual([
      ['gates', 8, 4, 4, false],
      ['floating', 16, 2, 14, false],
      ["not engineering's", 8, 1, 7, true],
    ]);
  });

  // Backlog 5608c1a2: a run's claim is taken from its queue station
  // (q.platform-admin.task, outside the tree), but its host vCPU is drawn
  // from the share its bill names — engineering's floating — so that is
  // where the page counts it. Its robot slot stays at the claim's station.
  test('a run claim’s host draw counts in the floating share its bill names, not at its queue station', () => {
    const run = heldClaim('j-run', 'q.platform-admin.task', {
      allocations: [{ pool: 'host:w-1', version: 2, amount: '2.5' }, { pool: 'robots:opus', version: 2, amount: '1' }],
      resource_acquisition: { workflow_kind: 'agent-run', bill: { model: 'opus', vcpu: '2.5', host: { node: 'w-1', station: 'engineering' } } },
    });
    const claims = parsePageClaims(claimsRead([run, heldClaim('j-gate', 'gates', { allocations: [{ pool: 'host:w-1', version: 1, amount: '4' }] })]));
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: claims.card } }), 'host:w-1');
    expect(b.inUseNote).toBeNull();
    expect(segs(b)).toEqual([
      ['gates', 8, 4, 4, false],
      ['floating', 16, 2.5, 13.5, false],
      ["not engineering's", 8, 0, 8, true],
    ]);
    expect(claims.card.claims[0]?.allocations).toEqual([
      { pool: 'host:w-1', amount: 2.5, station: 'engineering' },
      { pool: 'robots:opus', amount: 1, station: 'q.platform-admin.task' },
    ]);
  });

  // Backlog f23ce786: a claim recording no allocation beside one that
  // records 3 is not "3 in use"; every segment stays open and the band
  // counts both.
  test('one claim recording no allocation beside a recorded one leaves every segment unknown, and the band counts both', () => {
    const claims = parsePageClaims(claimsRead([
      heldClaim('a', 'gates', { allocations: [{ pool: 'host:w-1', version: 1, amount: '3' }] }),
      heldClaim('b', 'gates'),
    ]));
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: claims.card } }), 'host:w-1');
    expect(b.inUseNote).toBe('in use not recorded');
    expect(b.segments.every((s) => s.inUse === null && s.free === null)).toBe(true);
    expect(b.notes).toContain('1 recorded, 1 unrecorded');

    // The control: the recorded claim alone gives the definite figure.
    const known = allocated(['a', 'gates', 'host:w-1', '3']);
    const k = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: known.card } }), 'host:w-1');
    expect(k.inUseNote).toBeNull();
    expect(segs(k)[0]).toEqual(['gates', 8, 3, 5, false]);
    expect(k.notes.some((n) => n.includes('unrecorded'))).toBe(false);
  });

  test('one claim the record cannot place makes every segment unplaceable, never a smaller figure', () => {
    const claims = allocated(['a', 'gates', 'host:w-1', '4'], ['b', null, 'host:w-1', '2']);
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: claims.card } }), 'host:w-1');
    expect(b.inUseNote).toBe('in use not recorded');
    expect(b.segments.every((s) => s.inUse === null && s.free === null)).toBe(true);
  });

  test('a shared root pool the station draws on: its draw, the other stations’ draw hatched, and what is free at the root', () => {
    const claims = allocated(['a', 'gates', 'robots:opus', '1'], ['b', 'design-review', 'robots:opus', '1']);
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: claims.card } }), 'robots:opus');
    expect(segs(b)).toEqual([
      ['engineering', 1, 1, 0, false],
      ['other stations', 1, 1, 0, true],
      ['free at the root', 1, 0, 1, false],
    ]);
  });

  // Backlog d05e29a7: the page reads only its own subtree's claims, so
  // what stations outside it use is not in the read — said, never zero.
  test('a read narrowed to the station leaves the use outside it unread, never zero', () => {
    const narrowed = (c: ReturnType<typeof allocated>) => ({ ...c.card, within: 'engineering' });
    const held = allocated(['a', 'gates', 'host:w-1', '4'], ['b', 'engineering', 'host:w-1', '2']);
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: narrowed(held) } }), 'host:w-1');
    expect(segs(b)).toEqual([
      ['gates', 8, 4, 4, false],
      ['floating', 16, 2, 14, false],
      ["not engineering's", 8, null, null, true],
    ]);
    const drawn = allocated(['a', 'gates', 'robots:opus', '1']);
    const r = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'ready', data: narrowed(drawn) } }), 'robots:opus');
    expect(segs(r)).toEqual([
      ['engineering', 1, 1, 0, false],
      ['other stations', null, null, null, true],
      ['free at the root', null, null, null, false],
    ]);
  });

  test('the root: each department its share and the root slack; a pool of unknown capacity says so', () => {
    const bands = constraintBand(tree(), 'company', { load: { kind: 'loading' }, claims: { kind: 'loading' } });
    expect(segs(band(bands, 'host:w-1'))).toEqual([
      ['engineering', 24, null, null, false],
      ['root slack', 8, null, null, false],
    ]);
    const disk = band(bands, 'disk:ci-workspaces');
    expect(disk.capacity).toBeNull();
    expect(bands.map((x) => x.pool)).toEqual(['host:w-1', 'host:w-2', 'people:platform-admin', 'robots:opus', 'disk:ci-workspaces']);
  });

  test('a failed claims read keeps what the tree holds and draws no in-use figure', () => {
    const b = band(constraintBand(tree(), 'engineering', { load: { kind: 'loading' }, claims: { kind: 'failed', error: 'HTTP 503' } }), 'host:w-1');
    expect(b.inUseNote).toBe('in use unread');
    expect(b.segments.map((s) => s.amount)).toEqual([8, 16, 8]);
    expect(b.segments.every((s) => s.inUse === null && s.free === null)).toBe(true);
  });
});

describe('the page', () => {
  test('a failed read is red with no figures', () => {
    expect(page('engineering').card.plate.state).toBe('clear');
    const failed = page('engineering', reads({ claims: { kind: 'failed', error: '/api/claims: HTTP 503' } }));
    expect(failed.card.rail.mark).toBe('failed');
    expect(failed.card.plate.word).toBe('read failed');
  });

  test('the parts: each sub-station as its own card, and the station’s own machines with what they hold', () => {
    expect(page('engineering').parts.stations).toEqual(['forge', 'gates', 'loading-dock', 'probe-shed']);
    expect(page('gates').parts.machines).toEqual([{ name: 'automation:gate-runner', holding: 1 }]);
    expect(page('forge').parts.stations).toEqual(['forge-ci']);
  });

  // Design 08d1530e item 8 (backlog 950ad972): In read 1.46 MB of queue
  // to show five titles and Out was always "no departures read yet";
  // both went, and the page takes no queue read.
  test('the page has no In and no Out, and its model names no queue', () => {
    const m = page('engineering');
    expect(Object.keys(m)).not.toContain('columns');
    expect(JSON.stringify(m)).not.toContain('no departures read yet');
    expect(Object.keys(reads())).toEqual(['load', 'claims']);
  });

  test('Working: claims inside the station, grouped by the part holding them, each with its holder mark', () => {
    const m = page('engineering');
    if (m.working.state !== 'ready') throw new Error('working not ready');
    const groups = m.working.groups;
    expect(groups.map((g) => g.part)).toEqual(['engineering', 'forge', 'gates']);
    const here = groups[0]!;
    // The claim carries no title: its protocol stands in. Its time is
    // the instant, drawn local and UTC by the page (0b1fd59f).
    expect(here.rows[0]).toEqual({ key: 'j-build-1:s-j-build-1', mark: HOLDER_MARK.robot, holder: 'agent-claude', what: 'agent-run', since: '2026-11-08T07:00:00Z', href: '/ux/jobs/j-build-1' });
    expect(groups[1]!.rows[0]?.mark).toBe('■');
    expect(groups[1]!.rows[0]?.what).toBe('a packet');
    // The conductor's claim recorded before the history rebuild is said, not placed.
    expect(m.working.unplaced).toBe(1);
    const many = Array.from({ length: COLUMN_MAX + 3 }, (_, i) => heldClaim(`g-${i}`, 'gates'));
    const big = page('engineering', reads({ claims: { kind: 'ready', data: parsePageClaims(claimsRead(many)) } }));
    if (big.working.state !== 'ready') throw new Error('working not ready');
    expect(big.working.groups[0]?.rows).toHaveLength(COLUMN_MAX);
    expect(big.working.groups[0]?.more).toBe(3);
  });

  test('Working: two claims of one job at distinct steps in one part are two rows with distinct keys', () => {
    const two = [
      heldClaim('same-job', 'gates', { step_id: 'step-a', holder: 'agent-claude' }),
      heldClaim('same-job', 'gates', { step_id: 'step-b', holder: 'agent-codex' }),
    ];
    const m = page('engineering', reads({ claims: { kind: 'ready', data: parsePageClaims(claimsRead(two)) } }));
    if (m.working.state !== 'ready') throw new Error('working not ready');
    const rows = m.working.groups.find((g) => g.part === 'gates')!.rows;
    expect(rows.map((r) => r.key)).toEqual(['same-job:step-a', 'same-job:step-b']);
    // The job link stays the job's, whichever step holds it.
    expect(rows.map((r) => r.href)).toEqual(['/ux/jobs/same-job', '/ux/jobs/same-job']);
  });

  test('a failed claims read: Working draws nothing and says why', () => {
    const m = page('engineering', reads({ claims: { kind: 'failed', error: '/api/claims: HTTP 503' } }));
    expect(m.working).toEqual({ state: 'failed', error: '/api/claims: HTTP 503' });
  });

  test('a station the read does not hold has no page', () => {
    expect(stationPage(tree(), 'nowhere', reads())).toBeNull();
  });
});
