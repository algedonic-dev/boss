// Shared test helpers for the smoke suite.
//
// Per-page specs should stay terse — extract anything reused
// across files into here. See COVERAGE.md for conventions.

import type { Locator, Page } from '@playwright/test';
import { expect } from '@playwright/test';

/**
 * Pin the dev-server's persona to a specific employee. Two layers:
 *
 *  1. `boss-persona` cookie — the dev-server's API proxy reads it
 *     to synthesise the `x-boss-user` header for every backend
 *     hit, so the API responses are scoped to that employee.
 *  2. `boss.persona.empId` localStorage value — the SPA's
 *     `loadSession` reads this in DEMO_MODE to pick the rendered
 *     `session.value.user`. Without it the SPA still falls back
 *     to `roster[0]` (or `emp-001`), so any UI gated on
 *     `session.value.user.id` keeps showing the wrong employee.
 *
 * Both are wired here. Call before any `page.goto(...)` so the
 * first request lands with the cookie + the SPA hydrates with the
 * right session.
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
  await page.addInitScript((empId) => {
    try {
      localStorage.setItem('boss.persona.empId', empId);
    } catch {
      // No-op when localStorage isn't available (extension contexts etc.)
    }
  }, employeeId);
}

/**
 * Mount a page and wait for the AppShell + the page-level h1 to
 * render. Returns once the SPA's first paint has settled, so
 * subsequent role lookups don't race against hydration.
 */
export async function mountPage(
  page: Page,
  path: string,
  opts: { titleMatch?: RegExp } = {},
): Promise<void> {
  await page.goto(path);
  // AppShell renders for every authed route. If it never paints,
  // either auth failed or the bundle crashed.
  await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
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
