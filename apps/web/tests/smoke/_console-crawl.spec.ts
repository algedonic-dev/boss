// One-shot crawler: visit the main routes and report any
// console.error / pageerror / failed HTTP response. Not part of
// the smoke gate — run with:
//
//   bunx playwright test tests/smoke/_console-crawl.spec.ts --reporter=list
//
// The leading underscore + .spec.ts naming keeps it discoverable but
// out of the default suite (excluded via testIgnore in
// playwright.config.ts when we wire that, otherwise just opt in).

import { test } from '@playwright/test';
import { pinPersona } from './_helpers';

const ROUTES = [
  '/',
  '/me',
  '/inbox',
  '/jobs',
  '/accounts',
  '/people',
  '/parts',
  '/vendors',
  '/finance',
  '/shipping',
  '/assets',
  '/marketing-assets',
  '/calendar',
  '/sim',
  '/exec',
  '/cto',
  '/atlas',
  '/design',
  '/admin/job-kinds',
  '/admin/step-plugins',
  '/admin/policy',
  '/perf',
  '/events',
  '/warehouse',
  '/catalog',
  '/manual',
  '/watchlist',
  '/shop',
];

type Issue = { route: string; kind: string; text: string };

test('crawl every route and report issues', async ({ page }) => {
  test.setTimeout(120_000);
  await pinPersona(page, 'emp-001');

  const issues: Issue[] = [];
  let currentRoute = '';

  page.on('console', (msg) => {
    if (msg.type() === 'error') {
      issues.push({ route: currentRoute, kind: 'console.error', text: msg.text() });
    }
  });
  page.on('pageerror', (err) => {
    issues.push({ route: currentRoute, kind: 'pageerror', text: err.message });
  });
  page.on('response', (resp) => {
    const s = resp.status();
    if (s >= 400) {
      issues.push({
        route: currentRoute,
        kind: 'http',
        text: `${s} ${resp.request().method()} ${resp.url()}`,
      });
    }
  });

  for (const r of ROUTES) {
    currentRoute = r;
    try {
      await page.goto(r, { waitUntil: 'networkidle', timeout: 15_000 });
      await page.waitForTimeout(600);
    } catch (e: any) {
      issues.push({ route: r, kind: 'nav', text: e.message });
    }
  }

  // Pretty-print grouped by route. Always passes — this is a probe,
  // not a gate.
  const byRoute = new Map<string, Issue[]>();
  for (const i of issues) {
    if (!byRoute.has(i.route)) byRoute.set(i.route, []);
    byRoute.get(i.route)!.push(i);
  }
  console.log(`\n=== ${ROUTES.length} routes / ${issues.length} issues ===\n`);
  for (const r of ROUTES) {
    const list = byRoute.get(r) ?? [];
    if (list.length === 0) {
      console.log(`OK   ${r}`);
      continue;
    }
    console.log(`FAIL ${r}  (${list.length})`);
    const seen = new Set<string>();
    for (const i of list) {
      const key = `${i.kind}|${i.text}`;
      if (seen.has(key)) continue;
      seen.add(key);
      console.log(`     [${i.kind}] ${i.text.slice(0, 240)}`);
    }
  }
});
