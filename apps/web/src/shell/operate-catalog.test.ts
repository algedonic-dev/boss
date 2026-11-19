import { expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { ROUTE_CATALOG, departmentRows } from './nav-catalog';
import { parseRoute } from '../router';
import { ROUTES, RETIRED_ROUTES } from '../../tests/mocked/_routes';

// The actual current tab declaration is the roster. A served tab must
// reach the page-audit catalog as well as the browser crawl (e52e3017).
const source = readFileSync(new URL('../it/ItTabs.svelte', import.meta.url), 'utf8');
const tabs = [...source.matchAll(/path: '(\/it\/operate(?:\/[^']*)?)'/g)].map((m) => m[1]!);
const catalog = Object.values(ROUTE_CATALOG);

// Five until step 1 of design 01fa5bdf took Incidents, Atlas and
// Bottlenecks off the strip (2026-11-17).
test('the fixture reads the actual two Operate tabs', () => {
  expect(tabs).toEqual(['/it/operate/audit', '/it/operate/perf']);
});

for (const path of tabs) {
  test(`served tab ${path} has one owned page-audit entry and a crawl`, () => {
    const matches = catalog.filter((row) => row.path === path);
    expect(matches).toHaveLength(1);
    expect(matches[0]!.owner).toBe('it');
    expect(parseRoute(path).kind).not.toBe('notFound');
    expect(ROUTES).toContain(path);
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
    expect({ path, route: parseRoute(path).kind }).toEqual({ path, route: 'notFound' });
  }
});

test('the Audit Log is the Operate row and Performance stays a tab behind it', () => {
  const paths = departmentRows('it').map((row) => row.path);
  expect(paths).toContain('/it/operate/audit');
  expect(paths).not.toContain('/it/operate/perf');
});
