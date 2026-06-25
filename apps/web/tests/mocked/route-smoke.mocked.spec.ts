// Route smoke harness — the gated catch-all for "page renders real data
// and throws". Crawls every top-level surface against an adversarial
// mocked backend (see _smokeMocks.ts) and FAILS on any uncaught
// exception (pageerror) or a shell that never paints. This is the layer
// that was missing when StepDagEditor (omitted `terminal`) and the
// marketing-assets page crashed in the browser: pure-logic unit tests
// never mount the component, and svelte-check passes when the *type* is
// the thing that's wrong.
//
// Run: bunx playwright test -c playwright.mocked.config.ts tests/mocked/route-smoke.mocked.spec.ts

import { test, expect } from '@playwright/test';
import { installSmokeMocks } from './_smokeMocks';
import { installAuthoringMocks, JOB_ID } from './_mockApi';

// Every top-level surface a ceo persona reaches, from the router's
// exact-match routes. Pure-action / form-submit routes (/login,
// /finance/new, /finance/journal-entries/new) are excluded — this asserts
// surfaces RENDER without throwing, not that forms submit. Two detail
// routes are included (a JobKind + a marketing asset) because the mock
// seeds them, and that is where the omitted-field crashes live.
const ROUTES: ReadonlyArray<string> = [
  // User Experiences perspective — bare / is the public home alias; the
  // operator surfaces are re-rooted under /ux/*.
  '/', '/ux/me', '/ux/inbox', '/ux/jobs', '/ux/accounts', '/ux/vendors', '/ux/people', '/ux/parts',
  '/ux/products', '/ux/shipping', '/ux/assets', '/ux/catalog',
  '/ux/marketing-assets', '/ux/marketing-assets/ma-1', '/ux/calendar', '/ux/calendar/me',
  '/ux/support', '/ux/service', '/ux/refurb', '/ux/qa', '/ux/hr', '/ux/sales',
  '/ux/shop', '/ux/manual',
  // System Model perspective — the "read the running model" surfaces, now
  // re-rooted under /system/*.
  '/system', '/system/subjects', '/system/dispatcher', '/system/dispatcher/rules',
  '/system/monitoring/perf', '/system/monitoring/events',
  '/system/monitoring/atlas', '/system/step-plugins', '/system/kb', '/system/design',
  // Modeling + admin surfaces (System Model).
  '/system/workflows', '/system/job-kinds', '/system/job-kinds/new',
  '/system/job-kinds/seasonal-release', '/system/policy', '/system/auth-admin',
];

// DEFERRED, group 1 — aggregation dashboards that read OBJECT-shaped
// responses (statements, snapshots, summaries) the generic `[]` catch-all
// can't fake; they need faithful per-endpoint fixtures before they can be
// gated without false positives:
//   /ux/finance (statements .reduce) · /ux/warehouse (summary.below_reorder_count)
//   /ux/exec (.find/.length) · /ux/watchlist (.length) · /system/monitoring (snapshot .length)
//
// Resolved: the marketing-assets no-shell this harness first caught was a
// real effect_update_depth_exceeded loop in loadClasses() called from a
// tracked $effect — fixed in session/classes.svelte.ts. The /calendar +
// /calendar/me failures seen alongside it were that loop bleeding across a
// shared page (pre-fix); both render cleanly in isolation and are gated
// above. All four routes are now in ROUTES.

type Issue = { route: string; kind: string; text: string };

test.describe('route smoke — every surface renders without a runtime crash', () => {
  test('crawl all top-level routes (adversarial mocked backend)', async ({ page }) => {
    test.setTimeout(240_000);
    // ONE shared page: the browser HTTP cache keeps the (on-the-fly
    // bundled) dev-server JS warm, so each route reloads fast and the
    // shell paints reliably. A full goto per route reloads the document,
    // wiping the previous route's JS state — so there's no effect/timer
    // bleed despite sharing the page. `page.route` handlers persist
    // across navigations, so the mocks are installed once.
    await installSmokeMocks(page);

    const issues: Issue[] = [];
    let route = '';
    page.on('pageerror', (e) => issues.push({ route, kind: 'pageerror', text: e.message }));
    page.on('console', (m) => {
      if (m.type() === 'error') issues.push({ route, kind: 'console.error', text: m.text() });
    });

    for (const r of ROUTES) {
      route = r;
      // Retry the shell check once: the on-the-fly bundling dev-server
      // occasionally serves a route slowly enough that the shell misses
      // the window; a reload settles it. A genuine render failure misses
      // BOTH attempts (and a real crash still fires its pageerror, which
      // is recorded regardless of the shell timing).
      let shellOk = false;
      for (let attempt = 1; attempt <= 2 && !shellOk; attempt++) {
        try {
          // 'commit' (not 'load'/'domcontentloaded'): for a client-routed
          // SPA we only need the navigation to commit; the real readiness
          // signal is the AppShell painting, asserted next.
          await page.goto(r, { waitUntil: 'commit', timeout: 20_000 });
          await expect(page.locator('.app-shell')).toBeVisible({ timeout: 20_000 });
          shellOk = true;
        } catch (e) {
          if (attempt === 2) {
            issues.push({ route: r, kind: 'no-shell', text: e instanceof Error ? e.message : String(e) });
          }
        }
      }
      // Let onMount effects + the (instant) mocked fetches settle so any
      // data-render crash fires while we're listening.
      if (shellOk) await page.waitForTimeout(500);
    }

    // Gate on crashes: uncaught exceptions + shells that never painted.
    // console.error is reported but not gated (the adversarial empty
    // fixtures provoke benign error logs on some pages).
    const crashes = issues.filter((i) => i.kind !== 'console.error');
    const noise = issues.filter((i) => i.kind === 'console.error');
    if (noise.length) {
      console.log(`\nconsole.error (non-gating, ${noise.length}):`);
      for (const i of noise) console.log(`  [${i.route}] ${i.text}`);
    }
    expect(
      crashes,
      `Runtime crashes across ${ROUTES.length} routes:\n` +
        crashes.map((i) => `  [${i.route}] ${i.kind}: ${i.text}`).join('\n'),
    ).toEqual([]);
  });

  test('JobKind authoring workspace renders a serde-omitted terminal (StepDagEditor)', async ({ page }) => {
    test.setTimeout(60_000);
    // _mockApi.seedSpec() now omits `terminal` on the non-terminal step —
    // the exact shape that crashed StepDagEditor before the fix.
    await installAuthoringMocks(page);

    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto(`/system/job-kinds/authoring/${JOB_ID}`, { timeout: 20_000 });
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    // Wait for the lazy graph + the step-authoring surface (which mounts
    // StepDagEditor) to render the seeded spec.
    await page.waitForTimeout(2_000);

    expect(errors, `pageerrors in the authoring workspace:\n${errors.join('\n')}`).toEqual([]);
  });
});
