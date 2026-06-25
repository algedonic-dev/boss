// Lint test: every sidebar path defined in AppShell.svelte must be
// matched by a non-catch-all branch in router.ts.
//
// Why this exists: the router has a catch-all
// `return { kind: 'home' }` at the end of parseRoute(). When a
// sidebar path doesn't match any earlier branch, clicking it
// silently renders the home page (which itself falls through to
// MePage in App.svelte) — no console error, no 404, just the
// wrong page. The /schedule bug on 2026-05-22 was exactly this:
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
// Why a hand-maintained list instead of parsing AppShell.svelte:
// the Svelte file embeds the nav as a TypeScript const inside a
// `<script>` block; parsing it from a Bun test is fragile (the
// nav definition mixes labels, paths, permKeys, modules; format
// drift breaks the parser). Two files of truth with a mirroring
// test is simpler than a brittle parser, and the test failure
// message tells you exactly what to fix.

import { describe, it, expect, beforeAll, afterAll } from 'bun:test';
import { parseRoute } from '../router';

// parseRoute touches `window.location.search` inside its `/jobs`
// branch. Stub a minimal Location for tests so we don't need
// happy-dom for one property access.
const originalWindow = (globalThis as { window?: unknown }).window;
beforeAll(() => {
  (globalThis as { window?: { location: { search: string; pathname: string } } }).window = {
    location: { search: '', pathname: '/' },
  };
});
afterAll(() => {
  (globalThis as { window?: unknown }).window = originalWindow;
});

// Mirror of `ALL_NAV` paths from apps/web/src/shell/AppShell.svelte.
// Keep in sync — see file header.
const SIDEBAR_PATHS: ReadonlyArray<string> = [
  '/ux/jobs',
  '/ux/sales',
  '/ux/service',
  '/ux/refurb',
  '/ux/qa',
  '/ux/finance',
  '/ux/warehouse',
  '/ux/shipping',
  '/ux/support',
  '/ux/ops',
  '/ux/exec',
  '/system/monitoring',
  '/system/monitoring/events', // "Audit Log" — plain sub-page link in the Run group
  '/system/monitoring/atlas', // "Atlas" — plain sub-page link in the Run group
  '/ux/calendar/me', // "My schedule" — was /schedule (bug fixed 2026-05-22)
  '/ux/catalog',
  '/ux/parts',
  '/ux/products',
  '/ux/accounts',
  '/ux/vendors',
  '/ux/people',
  '/ux/assets',
  '/ux/shop',
  '/ux/inbox',
  '/ux/marketing-assets',
  '/ux/calendar',
  '/system/policy',
  // '/system/job-kinds' removed: "Job kinds" is no longer a sidebar
  // entry (Workflows is the single JobKind surface; authoring is
  // reached from there). The route itself still resolves.
  '/system/step-plugins',
  '/system/dispatcher',
  '/system/kb',
  '/system/auth-admin',
  '/system/workflows',
  '/system',
  '/system/subjects',
  '/system/design',
  '/system/experiments',
  '/ux/manual',
  '/ux/me',
];

describe('sidebar-router consistency', () => {
  for (const path of SIDEBAR_PATHS) {
    it(`sidebar path "${path}" resolves to a non-catch-all route`, () => {
      const route = parseRoute(path);
      // The catch-all returns { kind: 'home' }. If a sidebar path
      // intentionally lands on home, that's a configuration smell
      // — the home view has its own entry point ("/"), and any
      // *labeled* sidebar item should resolve to its own route.
      expect(
        route.kind,
        `sidebar path "${path}" fell through to the catch-all '{kind: "home"}' — ` +
          `router.ts has no branch matching it. Either add a branch to parseRoute() ` +
          `or repoint the sidebar entry in AppShell.svelte's ALL_NAV.`,
      ).not.toBe('home');
    });
  }

  it('the catch-all itself still works (regression: parseRoute returns home for unknown paths)', () => {
    expect(parseRoute('/definitely-not-a-real-route-xyzzy').kind).toBe('home');
  });
});
