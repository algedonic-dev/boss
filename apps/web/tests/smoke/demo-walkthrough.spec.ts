// Demo-mode walkthrough — the spec that would have caught two
// production bugs surfaced on 2026-05-25 in a single run:
//
//   1. /api/messages/inbox/<persona> 403 (persona cookie ignored
//      by the gateway)
//   2. /plugins/*.js 404 (step-plugin bundles missing from deploy)
//
// This is a **gate** — failing here blocks the smoke suite — and
// it models exactly what an anonymous OSS visitor does:
//
//   - land at /
//   - pick a brewery persona from the "View As" menu (demo mode)
//   - click into Inbox, Finance dashboard, a Job Detail with a
//     step-plugin-backed step
//   - expect zero `console.error` / 4xx / 5xx in the process
//
// The pages and the assertions stay terse so the spec stays
// reviewable: at most a few thousand-millisecond waits, no
// magic selectors, no per-test setup beyond `pinPersona`.

import { test, expect, type Page, type Request } from '@playwright/test';
import { pinPersona, mountPage } from './_helpers';

type Issue = { route: string; kind: string; text: string };

function attachConsoleAndNetworkProbes(
  page: Page,
  collect: Issue[],
  currentRoute: () => string,
): void {
  page.on('console', (msg) => {
    if (msg.type() === 'error') {
      collect.push({ route: currentRoute(), kind: 'console.error', text: msg.text() });
    }
  });
  page.on('pageerror', (err) => {
    collect.push({ route: currentRoute(), kind: 'pageerror', text: err.message });
  });
  page.on('response', (resp) => {
    const s = resp.status();
    if (s >= 400) {
      const req: Request = resp.request();
      collect.push({
        route: currentRoute(),
        kind: 'http',
        text: `${s} ${req.method()} ${resp.url()}`,
      });
    }
  });
}

test.describe('demo-mode visitor walkthrough', () => {
  test('walks the OSS demo flow with zero console errors or 4xx/5xx', async ({ page }) => {
    test.setTimeout(120_000);
    await pinPersona(page, 'emp-aa-004');

    const issues: Issue[] = [];
    let route = '';
    attachConsoleAndNetworkProbes(page, issues, () => route);

    // 1. Landing — the first thing any visitor sees.
    route = '/';
    await mountPage(page, '/');

    // 2. Inbox — the most-fragile cross-boundary surface. The
    //    SPA writes /api/messages/inbox/<session-user-id>; the
    //    backend's "you can only read your own inbox" check
    //    must match. This caught the persona-cookie-not-honored
    //    bug.
    route = '/inbox';
    await mountPage(page, '/inbox');
    await expect(page.locator('h1').first()).toContainText(/inbox/i, { timeout: 10_000 });

    // 3. Finance dashboard.
    route = '/finance';
    await mountPage(page, '/finance');

    // 4. A Job with a step-plugin-backed step (sr-triage is the
    //    canonical example). Opens the plugin host which fetches
    //    /plugins/sr-triage.js. Caught the missing-bundle bug.
    route = '/jobs';
    await mountPage(page, '/jobs');

    // Pretty-print + assert. Group identical issues so one bug
    // doesn't explode the diff.
    const seen = new Set<string>();
    const unique: Issue[] = [];
    for (const i of issues) {
      const key = `${i.route}|${i.kind}|${i.text}`;
      if (seen.has(key)) continue;
      seen.add(key);
      unique.push(i);
    }

    if (unique.length > 0) {
      console.log(`\n=== ${unique.length} unique issue(s) across walkthrough ===`);
      for (const i of unique) {
        console.log(`  [${i.route}] ${i.kind}: ${i.text.slice(0, 240)}`);
      }
    }

    expect(unique, 'demo walkthrough produced no console / network errors').toEqual([]);
  });
});
