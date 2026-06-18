// Accounts list — Tier + State filter buttons, search input, and
// row navigation. Designed to pass whether the brewery seed has
// account events in audit_log or not — the controls themselves
// are the contract under test.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Accounts list — controls', () => {
  test('page mounts with header + filter sidebar', async ({ page }) => {
    await mountPage(page, '/accounts', { titleMatch: /accounts/i });
    // Filter sidebar always renders, regardless of data.
    await expect(page.locator('aside.catalog-filters')).toBeVisible({
      timeout: 5_000,
    });
  });

  test('Tier filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/accounts', { titleMatch: /accounts/i });

    // The "All" button is always rendered with at least the tier
    // group's first row. Click it explicitly to exercise the
    // handler even when no per-tier data exists.
    const tierGroup = page.locator('aside.catalog-filters .filter-group').nth(1);
    const allBtn = tierGroup.getByRole('button').first();
    await allBtn.click();
    await expect(allBtn).toHaveClass(/filter-btn-active/);

    // Per-tier buttons (platinum/gold/silver) are always rendered.
    for (const t of ['platinum', 'gold', 'silver']) {
      const btn = page.getByRole('button', { name: new RegExp(t, 'i') });
      await btn.click();
      await expect(btn).toHaveClass(/filter-btn-active/);
    }
  });

  test('Search input persists what the user types', async ({ page }) => {
    await mountPage(page, '/accounts', { titleMatch: /accounts/i });
    const search = page.locator('aside.catalog-filters input').first();
    await search.fill('zzz_no_account_matches_this_zzz');
    await expect(search).toHaveValue('zzz_no_account_matches_this_zzz');

    // With a deliberately bogus query the empty state always
    // renders — even when seed data exists.
    await expect(page.locator('.empty')).toContainText(/no accounts/i, {
      timeout: 5_000,
    });
  });

  test('row click navigates to /accounts/{id} when seeded', async ({ page }) => {
    await mountPage(page, '/accounts', { titleMatch: /accounts/i });
    const firstRow = page.locator('table tbody tr').first();
    if ((await firstRow.count()) === 0) {
      test.skip(true, 'no account rows in seed (audit_log gap)');
    }
    await Promise.all([
      page.waitForURL(/\/accounts\/[^/]+/, { timeout: 10_000 }),
      firstRow.click(),
    ]);
  });
});
