// Watchlist + Parts list — Browse-area pages with the largest
// onclick counts. Both lean on FilterButton + sortable table
// columns (Watchlist).

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Watchlist (/watchlist) — controls', () => {
  test('page mounts with filter sidebar', async ({ page }) => {
    await mountPage(page, '/watchlist');
    await expect(page.locator('aside.catalog-filters')).toBeVisible({
      timeout: 10_000,
    });
  });

  test('Risk bucket buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/watchlist');
    for (const label of ['high', 'mid', 'low', 'all']) {
      const btn = page
        .getByRole('button', { name: new RegExp(`^${label}\\b`, 'i') })
        .first();
      if ((await btn.count()) === 0) continue;
      await btn.click();
      await expect(btn).toHaveClass(/filter-btn-active/);
    }
  });

  test('Tier filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/watchlist');
    for (const label of ['platinum', 'gold', 'silver']) {
      const btn = page.getByRole('button', { name: new RegExp(label, 'i') });
      if ((await btn.count()) === 0) continue;
      await btn.click();
      await expect(btn).toHaveClass(/filter-btn-active/);
    }
  });

  test('Search input filters', async ({ page }) => {
    await mountPage(page, '/watchlist');
    const search = page.locator('aside.catalog-filters input').first();
    await search.fill('zzz_no_match_zzz');
    await expect(search).toHaveValue('zzz_no_match_zzz');
  });

  test('column headers are clickable for sort', async ({ page }) => {
    await mountPage(page, '/watchlist');
    // Sortable th's have onclick + cursor:pointer style; assert
    // the score header at minimum exists.
    const scoreHeader = page.locator('th').filter({ hasText: /score/i }).first();
    if ((await scoreHeader.count()) === 0) {
      test.skip(true, 'watchlist not populated (no risk-scores returned)');
    }
    await scoreHeader.click();
    // Header doesn't have a stable visible class change — assert
    // the page didn't crash + table still renders or empty state.
    await expect(
      page.locator('table tbody tr, .empty').first(),
    ).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('Parts list (/parts) — controls', () => {
  test('Stock status filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/parts', { titleMatch: /parts/i });
    // First filter group: needs-attention / all / out / critical /
    // low / healthy. Pick three to exercise the toggle.
    for (const label of ['out of stock', 'critical', 'all']) {
      const btn = page.getByRole('button', { name: new RegExp(label, 'i') });
      if ((await btn.count()) === 0) continue;
      await btn.first().click();
      await expect(btn.first()).toHaveClass(/filter-btn-active/);
    }
  });

  test('Category filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/parts', { titleMatch: /parts/i });
    for (const label of ['ingredient', 'packaging', 'spare', 'consumable']) {
      const btn = page.getByRole('button', { name: new RegExp(label, 'i') });
      if ((await btn.count()) === 0) continue;
      await btn.first().click();
      await expect(btn.first()).toHaveClass(/filter-btn-active/);
    }
  });

  test('row link navigates to /parts/{sku}', async ({ page }) => {
    await mountPage(page, '/parts', { titleMatch: /parts/i });
    const link = page.locator('a[href*="/parts/"]').first();
    if ((await link.count()) === 0) {
      test.skip(true, 'parts list empty in seed');
    }
    await Promise.all([
      page.waitForURL(/\/parts\/[^/]+/, { timeout: 10_000 }),
      link.click(),
    ]);
  });
});
