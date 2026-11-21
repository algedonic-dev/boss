import { describe, expect, test } from 'bun:test';
import { parseStationLoad } from '../marshalling/marshalling';
import { ADDRESSES, claimsRead, heldClaim, treeRead } from '../../../tests/fixtures/station-tree';
import { parseRoute } from '../../router';
import { parseTree, type StationTree } from './station-tree';
import {
  breadcrumb, parseBinding, parseHeldClaims, parsePageClaims, stationHref, stationPage, upstreamOf,
  type PageReads, type StationPageModel,
} from './station-page';

// THE STATION PAGE (design c4854fd4 E2, decided by David 2026-11-07
// 23:38Z; backlog 72d58e00; re-cut by design 08d1530e, backlog
// 950ad972). Its header: the breadcrumb, the walk upstream and the
// claims read. Its body is the live floor (station-floor.test.ts,
// backlog dd0eb058), which replaced the pool strips, the parts cards and
// Working. REAL, CURRENT reads only (David 2026-11-08 00:50Z) — a figure
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

describe('the page address', () => {
  // A station's page sits below the tree floor, which the company root
  // hosts since backlog 92bfcb04 (car 3 of design df0ecd8b), so it opens
  // at /tree/<name> and the /it/tree spelling answers nothing (no shims
  // before 1.0.0).
  test('a station opens at /tree/<name>; the floor keeps /tree; a view in the query is ignored', () => {
    expect(stationHref('engineering')).toBe('/tree/engineering');
    expect(stationHref('q.platform-admin.task')).toBe('/tree/q.platform-admin.task');
    expect(parseRoute('/tree/engineering', '', ADDRESSES)).toEqual({ kind: 'systemStation', station: 'engineering' });
    expect(parseRoute('/tree/gates', '?view=track', ADDRESSES)).toEqual({ kind: 'systemStation', station: 'gates' });
    expect(parseRoute('/tree/gates', '?view=nowhere', ADDRESSES)).toEqual({ kind: 'systemStation', station: 'gates' });
    expect(parseRoute('/tree/a%20b', '', ADDRESSES)).toEqual({ kind: 'systemStation', station: 'a b' });
    expect(parseRoute('/tree', '', ADDRESSES)).toEqual({ kind: 'systemTree' });
    expect(parseRoute('/tree/a/b', '', ADDRESSES).kind).toBe('notFound');
  });

  test('the /it/tree spelling of a station page answers nothing', () => {
    for (const path of ['/it/tree/engineering', '/it/tree/gates', '/it/tree/a%20b']) {
      expect(parseRoute(path, '', ADDRESSES)).toEqual({ kind: 'notFound', path });
    }
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
    expect(crumbs.map((c) => c.href)).toEqual(['/tree/company', '/tree/engineering', '/tree/gates']);
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
    expect(up).toEqual({ name: 'engineering', title: expect.any(String), href: '/tree/engineering' });
    expect(parseRoute(up!.href, '', ADDRESSES).kind).toBe('systemStation');
  });

  test('the root has no parent, so no walk upstream rather than a guessed one', () => {
    expect(upstreamOf(tree(), 'company')).toBeNull();
  });

  test('the page model carries it', () => {
    const model = stationPage(tree(), 'gates', reads());
    expect(model?.upstream?.href).toBe('/tree/engineering');
  });
});

describe('the page', () => {
  test('a failed read is red with no figures', () => {
    expect(page('engineering').card.plate.state).toBe('clear');
    const failed = page('engineering', reads({ claims: { kind: 'failed', error: '/api/claims: HTTP 503' } }));
    expect(failed.card.rail.mark).toBe('failed');
    expect(failed.card.plate.word).toBe('read failed');
  });

  test('a station the read does not hold has no page', () => {
    expect(stationPage(tree(), 'nowhere', reads())).toBeNull();
  });
});
