// Jobs list page — sidebar status filters + row navigation.
// Pairs with jobs-create.spec.ts (the create-form half of the
// page); together they cover every interactive control.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Jobs list — filter + navigation', () => {
  test('status filter buttons toggle active class', async ({ page }) => {
    await mountPage(page, '/jobs', { titleMatch: /jobs/i });

    // Click each status filter. They're always rendered as a
    // closed list of buttons.
    for (const label of ['Open', 'Blocked', 'Pending sign-off', 'Closed', 'All']) {
      const btn = page.getByRole('button', { name: new RegExp('^' + label + '$', 'i') });
      await btn.click();
      await expect(btn).toHaveClass(/filter-button-active/);
    }
  });

  test('All filter renames the subtitle to "any-status"', async ({ page }) => {
    await mountPage(page, '/jobs', { titleMatch: /jobs/i });

    await page.getByRole('button', { name: /^all$/i }).click();
    // Subtitle is "<n> any-status" once the All button is active.
    await expect(page.locator('.page-header, h1').locator('..').locator('p').first()).toContainText(
      /any-status/i,
      { timeout: 5_000 },
    );
  });

  test('row click navigates to job detail', async ({ page }) => {
    await mountPage(page, '/jobs', { titleMatch: /jobs/i });
    const firstRow = page.locator('table tbody tr').first();
    if ((await firstRow.count()) === 0) {
      test.skip(true, 'no jobs in seed');
    }
    await Promise.all([
      page.waitForURL(/\/jobs\/[a-f0-9-]+/, { timeout: 10_000 }),
      firstRow.locator('td.mono a').first().click(),
    ]);
  });
});
