// Detail-page crawl: for each list page, click into the first
// few rows and assert no console.error / pageerror / 4xx fires.
// Catches the class of bug where a list-page link lands on a
// detail page whose payload shape the page can't handle.
//
// Why this exists: on 2026-05-22 two such bugs slipped through:
//
//   - DevicePage.svelte: `{@const ls = d.extras as
//     EquipmentExtras}` — a cast that lied about null. Brewery's
//     BREW-BARREL-* system models have `extras = null`; first
//     `ls.wavelengths_nm` access threw `Cannot read properties
//     of null`.
//   - PartPage.svelte: only resolved parts via
//     `collectParts(models)` (device-shop satellite linkage).
//     Brewery seeds parts directly into `/api/catalog/parts`;
//     every brewery part 404'd on its detail page even though
//     the inventory list happily linked to it.
//
// Both would have been caught by clicking the first row of
// /catalog and /parts respectively. Existing detail-page tests
// (detail-pages.spec.ts) mount detail routes with hardcoded ids
// — they don't FOLLOW links from list pages, so they miss the
// "any row from the live list works" contract.
//
// This spec walks the live list, clicks the first row, asserts
// the detail page renders without runtime errors. Brewery-shape
// payloads (null extras, standalone parts table, BREW-BARREL-*
// SKUs) are exercised because the live list is what the brewery
// seed produced.

import { test, expect } from '@playwright/test';
import { pinPersona } from './_helpers';

// List → "the first row link selector on the list page". The
// detail page URL is whatever the link points at; we don't
// predict it (the test just clicks).
const LIST_PAGES: ReadonlyArray<{ list: string; rowLinkSelector: string }> = [
  // Catalog rows: each .device-card has an anchor → /catalog/{sku}.
  // This is the page that caught the DevicePage null-extras crash
  // for brewery BREW-BARREL-* SKUs.
  { list: '/catalog', rowLinkSelector: 'a[href*="/catalog/"]:not([href$="/catalog"])' },
  // Parts table rows link to /parts/{sku}. Caught the brewery
  // collectParts-only resolution gap.
  { list: '/parts', rowLinkSelector: 'a[href*="/parts/"]:not([href$="/parts"])' },
  // Fleet → /assets/{serial}
  { list: '/assets', rowLinkSelector: 'a[href*="/assets/"]:not([href$="/assets"])' },
  // Accounts → /accounts/{id}
  { list: '/accounts', rowLinkSelector: 'a[href*="/accounts/"]:not([href$="/accounts"])' },
  // Vendors → /vendors/{lookup}
  { list: '/vendors', rowLinkSelector: 'a[href*="/vendors/"]:not([href$="/vendors"])' },
  // People → /people/{empId}
  { list: '/people', rowLinkSelector: 'a[href*="/people/"]:not([href$="/people"])' },
  // Shipping → /shipments/{id} (note: not /shipping/{id})
  { list: '/shipping', rowLinkSelector: 'a[href*="/shipments/"]' },
  // Assets → /assets/{id}
  { list: '/assets', rowLinkSelector: 'a[href*="/assets/"]:not([href$="/assets"])' },
];

for (const { list, rowLinkSelector } of LIST_PAGES) {
  test(`click first row of ${list} → detail page renders without runtime errors`, async ({
    page,
  }) => {
    test.setTimeout(30_000);
    await pinPersona(page, 'emp-001');

    const consoleErrors: string[] = [];
    const pageErrors: string[] = [];
    const responseErrors: { status: number; url: string }[] = [];

    page.on('console', (msg) => {
      if (msg.type() === 'error') consoleErrors.push(msg.text());
    });
    page.on('pageerror', (err) => {
      pageErrors.push(err.message);
    });
    page.on('response', (resp) => {
      const s = resp.status();
      const u = resp.url();
      // Only care about 4xx on /api/* — other 4xx (Cloudflare assets,
      // favicon, etc.) are noise. Skip /api/files which intentionally
      // returns 200 + envelope for unconfigured deployments.
      if (s >= 400 && s < 500 && u.includes('/api/')) {
        responseErrors.push({ status: s, url: u });
      }
    });

    await page.goto(list, { waitUntil: 'domcontentloaded', timeout: 15_000 });
    // Let the list render its rows.
    await page.waitForTimeout(1500);

    // Find a row link. Some lists may be empty on a fresh seed —
    // skip rather than fail in that case.
    const firstRow = page.locator(rowLinkSelector).first();
    if ((await firstRow.count()) === 0) {
      test.skip(true, `no rows on ${list} — list empty in this seed`);
    }

    // Capture the URL before click so we can wait for it to change.
    const listUrl = page.url();
    await firstRow.click();
    // Wait for URL change (navigation happened) — networkidle is
    // too strict for pages with many concurrent CRM fetches.
    await page.waitForFunction((u) => window.location.href !== u, listUrl, {
      timeout: 5_000,
    });
    // Give the detail page a beat to mount and run its first effect
    // pass so any synchronous null-deref / API 4xx surfaces.
    await page.waitForTimeout(2000);

    // The detail page should have an h1 visible — proves the
    // route didn't fall through to a blank state.
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 5_000 });

    expect(
      { console: consoleErrors, page: pageErrors, http: responseErrors },
      `detail page rendered with runtime errors after clicking first row of ${list}`,
    ).toEqual({ console: [], page: [], http: [] });
  });
}
