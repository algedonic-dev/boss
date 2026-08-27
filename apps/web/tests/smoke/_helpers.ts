// Shared test helpers for the smoke suite.
//
// Per-page specs should stay terse — extract anything reused
// across files into here. See COVERAGE.md for conventions.

import type { Locator, Page } from '@playwright/test';
import { expect } from '@playwright/test';

/**
 * Pin the dev-server's persona to a specific employee.
 *
 * One layer: the `boss-persona` cookie. The dev-server's API proxy
 * reads it to synthesise the `x-boss-user` header for every backend
 * hit, so API responses — including `/api/session` — are scoped to
 * that employee. The SPA takes its identity from that session probe
 * and from nothing else: there is no client-side fallback that
 * invents a user, so an unpinned page renders unauthenticated rather
 * than quietly showing `roster[0]`.
 *
 * Call before any `page.goto(...)` so the first request already
 * carries the cookie.
 */
export async function pinPersona(page: Page, employeeId: string): Promise<void> {
  await page.context().addCookies([
    {
      name: 'boss-persona',
      value: employeeId,
      domain: '127.0.0.1',
      path: '/',
    },
  ]);
}

/**
 * Mount a page and wait for the AppShell + the page-level h1 to
 * render. Returns once the SPA's first paint has settled, so
 * subsequent role lookups don't race against hydration.
 */
export async function mountPage(
  page: Page,
  path: string,
  opts: { titleMatch?: RegExp; root?: string } = {},
): Promise<void> {
  await page.goto(path);
  // AppShell renders for every authed route — except the handful that
  // deliberately render outside it to take the whole viewport (login,
  // the full-page step surface). Those pass their own root; waiting
  // for `.app-shell` there fails on a page that is working correctly.
  await expect(page.locator(opts.root ?? '.app-shell')).toBeVisible({ timeout: 10_000 });
  if (opts.titleMatch) {
    await expect(page.locator('h1').first()).toContainText(opts.titleMatch, {
      timeout: 10_000,
    });
  }
}

/**
 * Click a button by its visible name (role + accessible name). Uses
 * the button's own ARIA name, falling back to its text content.
 * Asserts the button is enabled before clicking — catches the
 * common "test passed because the click was a no-op on a disabled
 * button" failure mode.
 */
export async function clickButton(
  page: Page | Locator,
  name: RegExp | string,
): Promise<void> {
  const btn = page.getByRole('button', { name });
  await expect(btn).toBeEnabled({ timeout: 5_000 });
  await btn.click();
}

/**
 * Assert that clicking the given button triggers a navigation to a
 * URL matching `urlMatch`. The classic "click → page changes"
 * smoke shape.
 */
export async function clickAndExpectNavigation(
  page: Page,
  buttonName: RegExp | string,
  urlMatch: RegExp,
): Promise<void> {
  await Promise.all([
    page.waitForURL(urlMatch, { timeout: 10_000 }),
    clickButton(page, buttonName),
  ]);
}

/**
 * Wait for the data table on a list page to populate. Returns the
 * first row locator so callers can chain row-level assertions.
 */
export async function expectTableRow(page: Page): Promise<Locator> {
  const firstRow = page.locator('table tbody tr').first();
  await expect(firstRow).toBeVisible({ timeout: 10_000 });
  return firstRow;
}
