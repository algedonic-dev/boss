// Admin · Job kinds — list page (row links) + detail page
// (Publish / Retire / Fork). The brewery seed pipeline registers
// JobKinds at startup, so these always have data to work with —
// no skip-on-empty branches needed.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Admin Job kinds list', () => {
  test('renders rows', async ({ page }) => {
    await mountPage(page, '/system/job-kinds', { titleMatch: /job kind/i });
    // The list is rendered as a table on this page.
    await expect(page.locator('table tbody tr').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('row link navigates to /system/job-kinds/{kind}', async ({ page }) => {
    await mountPage(page, '/system/job-kinds', { titleMatch: /job kind/i });
    const firstLink = page.locator('a[href*="/system/job-kinds/"]').first();
    await expect(firstLink).toBeVisible({ timeout: 10_000 });
    await Promise.all([
      page.waitForURL(/\/admin\/job-kinds\/[^/]+/, { timeout: 10_000 }),
      firstLink.click(),
    ]);
  });
});

test.describe('Admin Job kind detail — controls', () => {
  test('Edit + Retire buttons render with correct enabled state', async ({
    page,
  }) => {
    await mountPage(page, '/system/job-kinds/ad-hoc');
    await expect(page.locator('h1')).toBeVisible({ timeout: 10_000 });

    // D6 removed the direct "Publish draft" control — authoring/publish
    // now runs through a job-kind-design Job (opened via Edit…).
    const edit = page.getByRole('button', { name: /edit/i });
    const retire = page.getByRole('button', { name: /^retire$/i });
    await expect(edit).toBeVisible({ timeout: 5_000 });
    await expect(retire).toBeVisible();

    // Active kind → Edit (new version) + Retire both enabled.
    await expect(edit).toBeEnabled();
    await expect(retire).toBeEnabled();
  });

  test('Fork navigates to /system/job-kinds/new?fork=…', async ({ page }) => {
    await mountPage(page, '/system/job-kinds/ad-hoc');
    await expect(page.locator('h1')).toBeVisible({ timeout: 10_000 });

    await Promise.all([
      page.waitForURL(/\/admin\/job-kinds\/new\?fork=ad-hoc/, {
        timeout: 10_000,
      }),
      page.getByRole('button', { name: /fork/i }).click(),
    ]);
  });
});
