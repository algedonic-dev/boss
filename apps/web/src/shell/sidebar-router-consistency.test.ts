// Lint test: every sidebar path defined in AppShell.svelte must be
// matched by a non-catch-all branch in router.ts.
//
// Why this exists: the router has a catch-all at the end of
// parseRoute(). It returned `{ kind: 'home' }` until design ee3a3a2f,
// so a sidebar path that matched no earlier branch silently rendered
// the wrong page — no console error, no 404. It returns
// `{ kind: 'notFound' }` now and the page says so, but a labelled
// sidebar row that says "No page at this address" is still a broken
// row. The /schedule bug on 2026-05-22 was exactly this:
// sidebar `path: '/schedule'` but router only matched
// `/service/schedule`. The user clicked "My schedule" and landed
// on their profile page.
//
// Maintenance: when a sidebar entry is added or renamed in
// AppShell.svelte's `ALL_NAV` table, mirror the path here. The
// test fails on drift in either direction (sidebar adds a path
// router doesn't handle, or router removes a branch a sidebar
// path depended on).
//
// The path list is no longer hand-maintained: ROUTE_CATALOG moved out
// of AppShell.svelte into ./nav-catalog as a plain module, so this
// test imports the real registry. The previous version mirrored every
// path by hand (a Bun test can't reliably parse a TypeScript const out
// of a Svelte `<script>` block), which meant the drift-catching test
// could itself drift.

import { describe, it, expect } from 'bun:test';
import { parseRoute } from '../router';
import { ROUTE_CATALOG, type NavItem } from './nav-catalog';
import { pagePath } from './page-address';
import { ADDRESSES } from '../../tests/fixtures/station-tree';

// Every catalog path, straight from the registry the sidebar renders —
// a hosted page at its host station's address in the tree plus its slug
// (backlog 0bc2eb68), the fixture's tree standing in for the server's.
// Plus the always-available surface reachable outside the sidebar: the
// site root, which renders My Day in place. My Day itself is the
// catalog's `me` entry at /me since backlog 5cb56bb3, and '/ux/manual'
// answers nothing since the manual is hosted at Engineering.
const INLINE_SUBPAGE_PATHS: ReadonlyArray<string> = [
  '/',
];

const SIDEBAR_PATHS: ReadonlyArray<string> = [
  ...new Set([
    ...Object.values(ROUTE_CATALOG).map((i) => pagePath(i as NavItem, ADDRESSES) ?? `unplaced:${i.id}`),
    ...INLINE_SUBPAGE_PATHS,
  ]),
];

// Catalog paths describe parameters, not literal department codes or ids.
// Exercise every parameterised entry with the same valid segment; accepting
// the placeholder itself would loosen the production router's input contract.
function routeExample(path: string): string {
  return path.replace(/:[A-Za-z][A-Za-z0-9_]*/g, 'example');
}

describe('sidebar-router consistency', () => {
  for (const path of SIDEBAR_PATHS) {
    it(`sidebar path "${path}" resolves to a non-catch-all route`, () => {
      const route = parseRoute(routeExample(path), '', ADDRESSES);
      // The catch-all returns { kind: 'notFound' }; any *labeled*
      // sidebar item should resolve to its own route.
      expect(
        route.kind,
        `sidebar path "${path}" fell through to the catch-all '{kind: "notFound"}' — ` +
          `router.ts has no branch matching it. Either add a branch to parseRoute() ` +
          `or repoint the sidebar entry in nav-catalog.ts.`,
      ).not.toBe('notFound');
    });
  }

  it('materialises every parameter without hiding an unmatched route', () => {
    expect(routeExample('/unregistered/:code/child/:id')).toBe('/unregistered/example/child/example');
    expect(parseRoute(routeExample('/unregistered/:code/child/:id'), '', ADDRESSES)).toEqual({
      kind: 'notFound', path: '/unregistered/example/child/example',
    });
    expect(parseRoute('/it/floor/:code', '', ADDRESSES)).toEqual({ kind: 'notFound', path: '/it/floor/:code' });
  });

  it('the catch-all itself still works: an unknown path is notFound, naming it', () => {
    // Two segments: one bare segment is a department's address since car
    // 2 of design 8c3e9599, which the registry judges, not the router.
    expect(parseRoute('/definitely-not/a-real-route-xyzzy', '', ADDRESSES)).toEqual({
      kind: 'notFound',
      path: '/definitely-not/a-real-route-xyzzy',
    });
  });
});
