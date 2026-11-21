// THE TOP BAR WALKS THE STATION TREE; THE SIDEBAR IS WHAT THE STATION
// HOSTS (backlog 474e2c1c; David 2026-11-19: "Let's make the Top nav bar
// the navigation tool through the different stations. So the top bar
// will have \"Home\", and then it should show the path through the tree
// to let the user navigate back through the stations").
//
// Every assertion is the derivation the shell makes, against the shared
// fixture tree (the server's read, standing in), so a re-parented
// station is a fixture edit and nothing else.

import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { PHONE_QUERY } from '@boss/web-kit/ui/phone';
import { ROUTE_CATALOG, STATION, type NavItem } from './nav-catalog';
import { ADDRESSES_LOADING, addressesOf, pagePath, type StationAddresses } from './page-address';
import {
  MAX_FULL_TRAIL,
  sidebarOf,
  stationLabel,
  trailOf,
  trailView,
  viewedBy,
  type TrailStop,
  type Viewing,
} from './station-trail';
import { parseTree } from '../it/tree/station-tree';
import { stationHref } from '../it/tree/station-page';
import { ADDRESSES, TREE_ROWS, chainRows, idOf, treeRead } from '../../tests/fixtures/station-tree';
import { parseRoute, type Route } from '../router';

const ready = (at: StationAddresses) => {
  if (at.kind !== 'ready') throw new Error('the fixture tree did not read');
  return at;
};

const names = (stops: ReadonlyArray<TrailStop>): ReadonlyArray<string> => stops.map((s) => s.name);

/** The trail a route draws, in the fixture tree. */
const trailAt = (path: string, at: StationAddresses = ADDRESSES) => trailOf(viewedBy(parseRoute(path, '', at), at), at);

describe('what a route views', () => {
  test("the person's own pages view Home, never a station of the company", () => {
    for (const path of ['/me', '/me/inbox', '/me/calendar']) {
      expect(viewedBy(parseRoute(path, '', ADDRESSES), ADDRESSES)).toEqual({ kind: 'home' });
    }
  });

  test('a hosted page views the station that hosts it, by id', () => {
    expect(viewedBy(parseRoute('/finance/vendors', '', ADDRESSES), ADDRESSES)).toEqual({ kind: 'station', id: STATION.finance });
    expect(viewedBy(parseRoute('/engineering/registry/agents', '', ADDRESSES), ADDRESSES)).toEqual({ kind: 'station', id: STATION.engineering });
    expect(viewedBy(parseRoute('/operations/vault/credentials', '', ADDRESSES), ADDRESSES)).toEqual({ kind: 'station', id: STATION.vault });
  });

  test("a station's page views that station, by the name the route carries", () => {
    expect(viewedBy({ kind: 'systemStation', station: 'gates' }, ADDRESSES)).toEqual({ kind: 'station', id: idOf('gates') });
    expect(viewedBy({ kind: 'systemStationResources', station: 'forge' }, ADDRESSES)).toEqual({ kind: 'station', id: idOf('forge') });
    // At its address, as the router places it.
    expect(viewedBy(parseRoute('/engineering/gates', '', ADDRESSES), ADDRESSES)).toEqual({ kind: 'station', id: idOf('gates') });
  });

  test('a record view belongs to no one station, so it views the company root', () => {
    for (const r of [{ kind: 'jobDetail', jobId: 'j-1' }, { kind: 'search', q: 'x' }] as ReadonlyArray<Route>) {
      expect(viewedBy(r, ADDRESSES)).toEqual({ kind: 'root' });
    }
  });

  test("a station named by the route but not by the tree is said, not dropped", () => {
    expect(viewedBy({ kind: 'systemStation', station: 'nowhere' }, ADDRESSES)).toEqual({ kind: 'unplaced', name: 'nowhere' });
  });

  test('a station page asked before the tree answered waits for it', () => {
    expect(viewedBy({ kind: 'systemStation', station: 'gates' }, ADDRESSES_LOADING)).toEqual({ kind: 'unplaced', name: 'gates' });
    expect(trailOf({ kind: 'unplaced', name: 'gates' }, ADDRESSES_LOADING)).toEqual({ kind: 'loading' });
  });
});

describe('the trail: Home, then the path from the root to the station being viewed', () => {
  test('the root alone, current, on a page the root hosts', () => {
    const t = trailAt('/alarms');
    expect(t.kind).toBe('ready');
    if (t.kind !== 'ready') return;
    expect(t.home).toBe(false);
    expect(names(t.stops)).toEqual(['company']);
    expect(t.stops[0]!.current).toBe(true);
  });

  test("on the person's own pages Home is current and the root is one link away", () => {
    const t = trailAt('/me');
    expect(t).toMatchObject({ kind: 'ready', home: true });
    if (t.kind !== 'ready') return;
    expect(names(t.stops)).toEqual(['company']);
    expect(t.stops[0]!.current).toBe(false);
  });

  test('depth 1..3 in the shipped shape: every ancestor in order, the last current', () => {
    const cases: ReadonlyArray<[string, ReadonlyArray<string>]> = [
      ['/finance', ['company', 'finance']],
      ['/engineering/registry', ['company', 'engineering']],
      ['/engineering/design', ['company', 'engineering', 'design']],
      ['/engineering/gates', ['company', 'engineering', 'gates']],
    ];
    for (const [path, want] of cases) {
      const t = trailAt(path);
      expect(t.kind).toBe('ready');
      if (t.kind !== 'ready') continue;
      expect(names(t.stops)).toEqual(want);
      expect(t.stops.map((s) => s.current)).toEqual(want.map((_, i) => i === want.length - 1));
    }
  });

  test('depth 1..5 down a straight chain: each segment links to that ancestor at its address', () => {
    const at = ready(addressesOf(parseTree(treeRead('s0', chainRows(6)))));
    for (let depth = 1; depth <= 5; depth += 1) {
      const t = trailOf({ kind: 'station', id: idOf(`s${depth}`) }, at);
      expect(t.kind).toBe('ready');
      if (t.kind !== 'ready') continue;
      expect(names(t.stops)).toEqual(Array.from({ length: depth + 1 }, (_, i) => `s${i}`));
      // Every link is the address the router places that station at —
      // the same derivation the routes use, so a crumb cannot disagree
      // with a route. The root's own address is /, which renders the
      // person's day until it renders the root; its link is the root's
      // station page by name, the address the router gives it today.
      expect(t.stops[0]!.href).toBe(stationHref('s0'));
      for (let i = 1; i <= depth; i += 1) {
        expect(t.stops[i]!.href).toBe(at.stations.find((s) => s.name === `s${i}`)!.prefix);
      }
    }
  });

  test('a re-parented station moves its trail and its links with it', () => {
    const moved = TREE_ROWS.map((r) => (r.name === 'gates' ? { ...r, parent: 'operations' } : r));
    const at = addressesOf(parseTree(treeRead('company', moved)));
    const t = trailOf({ kind: 'station', id: idOf('gates') }, at);
    expect(t.kind).toBe('ready');
    if (t.kind !== 'ready') return;
    expect(names(t.stops)).toEqual(['company', 'operations', 'gates']);
    expect(t.stops.map((s) => s.href)).toEqual([stationHref('company'), '/operations', '/operations/gates']);
  });

  test('an unread tree says so — loading, or failed with why — never an empty trail', () => {
    expect(trailOf({ kind: 'station', id: STATION.finance }, ADDRESSES_LOADING)).toEqual({ kind: 'loading' });
    expect(trailOf({ kind: 'root' }, { kind: 'failed', why: 'GET /api/stations answered 503' })).toEqual({
      kind: 'failed',
      why: 'GET /api/stations answered 503',
    });
    // Home needs no tree to be Home, and still says the tree is unread.
    expect(trailOf({ kind: 'home' }, ADDRESSES_LOADING)).toEqual({ kind: 'loading' });
  });

  test('a station id the tree does not hold is said, never drawn as the root', () => {
    const t = trailOf({ kind: 'station', id: 'id-gone' }, ADDRESSES);
    expect(t).toEqual({ kind: 'failed', why: 'the station tree holds no station `id-gone`' });
    expect(trailOf({ kind: 'unplaced', name: 'nowhere' }, ADDRESSES)).toEqual({
      kind: 'failed',
      why: 'the station tree holds no station named `nowhere`',
    });
  });

  test("a stop's label is its title's own name, the whole title its hint", () => {
    expect(stationLabel({ name: 'engineering', title: 'Engineering — makes the change' })).toBe('Engineering');
    expect(stationLabel({ name: 'gates', title: '' })).toBe('gates');
  });
});

describe('the middle collapses on a phone, or when the path is deep', () => {
  const chain = (n: number): ReadonlyArray<TrailStop> =>
    Array.from({ length: n }, (_, i) => ({ id: `id-s${i}`, name: `s${i}`, label: `S${i}`, title: '', href: `/s${i}`, current: i === n - 1 }));

  test('wide and shallow: the whole path, nothing hidden', () => {
    for (let n = 1; n <= MAX_FULL_TRAIL; n += 1) {
      const v = trailView(chain(n), { narrow: false });
      expect(v.hidden).toEqual([]);
      expect(names([...v.head, ...v.tail])).toEqual(names(chain(n)));
    }
  });

  test('wide and deep: the root, an expander holding the middle in order, the current station', () => {
    const v = trailView(chain(MAX_FULL_TRAIL + 2), { narrow: false });
    expect(names(v.head)).toEqual(['s0']);
    expect(names(v.hidden)).toEqual(['s1', 's2', 's3', 's4']);
    expect(names(v.tail)).toEqual([`s${MAX_FULL_TRAIL + 1}`]);
  });

  test('on a phone any middle collapses; the root and the current station stay', () => {
    expect(trailView(chain(2), { narrow: true }).hidden).toEqual([]);
    const v = trailView(chain(4), { narrow: true });
    expect(names(v.head)).toEqual(['s0']);
    expect(names(v.hidden)).toEqual(['s1', 's2']);
    expect(names(v.tail)).toEqual(['s3']);
  });

  test('the bar turns over at the one phone width (CLAUDE.md 9a: the width lives in ui/phone.ts)', () => {
    const src = readFileSync(new URL('./StationTrail.svelte', import.meta.url), 'utf8');
    expect(src).toContain('matchMedia?.(PHONE_QUERY)');
    expect([...src.matchAll(/@media\s*(\([^)]*\))/g)].map((m) => m[1])).toEqual([PHONE_QUERY]);
  });

  test('nothing is lost by collapsing: head, hidden and tail are the path', () => {
    for (let n = 1; n <= 7; n += 1) {
      for (const narrow of [false, true]) {
        const v = trailView(chain(n), { narrow });
        expect(names([...v.head, ...v.hidden, ...v.tail])).toEqual(names(chain(n)));
      }
    }
  });
});

describe('the sidebar holds only what the station being viewed hosts', () => {
  const hostedAt = (id: string): ReadonlyArray<string> =>
    (Object.values(ROUTE_CATALOG) as ReadonlyArray<NavItem>).filter((e) => e.host === id && e.unlisted !== true).map((e) => e.id);

  test("a station's sidebar is exactly its declared hosted entries, in catalog order", () => {
    for (const id of Object.values(STATION)) {
      const s = sidebarOf({ kind: 'station', id }, ADDRESSES);
      expect(s.kind).toBe('ready');
      if (s.kind !== 'ready') continue;
      expect(s.items.map((i) => i.id)).toEqual([...hostedAt(id)]);
    }
    const fin = sidebarOf({ kind: 'station', id: STATION.finance }, ADDRESSES);
    expect(fin.kind === 'ready' ? fin.items.map((i) => i.id) : []).toEqual(['finance', 'vendors']);
  });

  test("Home's sidebar is the person's own pages and nothing of any station", () => {
    const s = sidebarOf({ kind: 'home' }, ADDRESSES_LOADING);
    expect(s.kind).toBe('ready');
    if (s.kind !== 'ready') return;
    expect(s.items.map((i) => i.id)).toEqual(['me', 'schedule', 'inbox']);
    expect(s.items.every((i) => i.person === true)).toBe(true);
  });

  test('the root views its own pages; a record view takes the root sidebar', () => {
    const s = sidebarOf({ kind: 'root' }, ADDRESSES);
    expect(s.kind === 'ready' ? s.items.map((i) => i.id) : []).toEqual([...hostedAt(STATION.algedonic)]);
  });

  test('no row belongs to a department: no Jobs row, no department group', () => {
    for (const v of [{ kind: 'home' }, { kind: 'root' }, ...Object.values(STATION).map((id) => ({ kind: 'station', id }))] as ReadonlyArray<Viewing>) {
      const s = sidebarOf(v, ADDRESSES);
      if (s.kind !== 'ready') continue;
      expect(s.items.map((i) => i.id)).not.toContain('department-jobs');
      expect(s.items.every((i) => i.person === true || i.host !== undefined)).toBe(true);
    }
  });

  test('every sidebar link is the address the router answers for it', () => {
    for (const id of Object.values(STATION)) {
      const s = sidebarOf({ kind: 'station', id }, ADDRESSES);
      if (s.kind !== 'ready') continue;
      for (const item of s.items) {
        const to = pagePath(item, ADDRESSES);
        expect(to).not.toBeNull();
        const r = parseRoute(to!, '', ADDRESSES);
        expect(r.kind).not.toBe('notFound');
        expect(viewedBy(r, ADDRESSES)).toEqual({ kind: 'station', id });
      }
    }
  });

  test("a station sidebar before the tree answers says so; the station's label waits on the tree", () => {
    expect(sidebarOf({ kind: 'station', id: STATION.finance }, ADDRESSES_LOADING)).toEqual({ kind: 'loading' });
    expect(sidebarOf({ kind: 'root' }, { kind: 'failed', why: 'x' })).toEqual({ kind: 'failed', why: 'x' });
  });

  test('the group is labelled with the station it belongs to', () => {
    const s = sidebarOf({ kind: 'station', id: STATION.engineering }, ADDRESSES);
    expect(s.kind === 'ready' && s.label).toBe(stationLabel(ready(ADDRESSES).stations.find((x) => x.id === STATION.engineering)!));
  });
});
