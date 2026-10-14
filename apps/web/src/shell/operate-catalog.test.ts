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

test('the fixture reads the actual five Operate tabs', () => {
  expect(tabs).toHaveLength(5);
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

test('retired Yard status and Conductor stay retired', () => {
  for (const path of ['/it/operate/yard-status', '/it/operate/conductor']) {
    expect(RETIRED_ROUTES).toContain(path);
    expect(ROUTES).not.toContain(path);
    expect(catalog.some((row) => row.path === path)).toBe(false);
    expect(parseRoute(path).kind).toBe('notFound');
  }
});

test('Performance and Atlas remain tabs rather than additional sidebar rows', () => {
  const paths = departmentRows('it').map((row) => row.path);
  expect(paths).not.toContain('/it/operate/perf');
  expect(paths).not.toContain('/it/operate/atlas');
});
