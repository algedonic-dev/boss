import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { ROUTE_CATALOG, STATION } from './nav-catalog';
import { sidebarOf } from './station-trail';
import { parseRoute } from '../router';
import { ROUTES, RETIRED_ROUTES } from '../../tests/mocked/_routes';
import { ADDRESSES, pageAt } from '../../tests/fixtures/station-tree';

// The actual current tab declaration is the roster. A served tab must
// reach the page-audit catalog as well as the browser crawl (e52e3017).
// Each tab names its page by catalog id since backlog 0bc2eb68 (a page's
// path is its host station's address plus its slug), so the roster is
// the ids in the strip's `operate` group.
const source = readFileSync(new URL('../it/ItTabs.svelte', import.meta.url), 'utf8');
const operate = source.slice(source.indexOf('operate: ['), source.indexOf('registry: ['));
const tabs = [...operate.matchAll(/page: '([^']+)'/g)].map((m) => m[1]! as keyof typeof ROUTE_CATALOG);
const catalog = Object.values(ROUTE_CATALOG);

// Five until step 1 of design 01fa5bdf took Incidents, Atlas and
// Bottlenecks off the strip (2026-11-17).
test('the fixture reads the actual two Operate tabs', () => {
  expect(tabs).toEqual(['system-monitoring', 'system-performance']);
});

for (const id of tabs) {
  test(`served tab ${id} has one owned page-audit entry and a crawl`, () => {
    const entry = ROUTE_CATALOG[id];
    expect(entry.owner).toBe('it');
    expect(parseRoute(pageAt(id), '', ADDRESSES).kind).not.toBe('notFound');
    expect(ROUTES).toContain(pageAt(id));
  });
}

test('retired Yard status, Conductor, Incidents, Atlas and Bottlenecks stay retired', () => {
  for (const path of [
    '/it/operate/yard-status', '/it/operate/conductor',
    // design 01fa5bdf step 1
    '/it/operate', '/it/operate/atlas', '/it/operate/bottlenecks',
  ]) {
    expect({ path, retired: RETIRED_ROUTES.includes(path) }).toEqual({ path, retired: true });
    expect({ path, crawled: ROUTES.includes(path) }).toEqual({ path, crawled: false });
    expect({ path, catalogued: catalog.some((row) => row.path === path) }).toEqual({ path, catalogued: false });
    expect({ path, route: parseRoute(path, '', ADDRESSES).kind }).toEqual({ path, route: 'notFound' });
  }
});

test('the Audit Log is the Operate row and Performance stays a tab behind it', () => {
  // The row sits in its host station's sidebar (backlog 474e2c1c).
  const s = sidebarOf({ kind: 'station', id: STATION.operations }, ADDRESSES);
  const ids = s.kind === 'ready' ? s.items.map((row) => row.id) : [];
  expect(ids).toContain('system-monitoring');
  expect(ids).not.toContain('system-performance');
});
