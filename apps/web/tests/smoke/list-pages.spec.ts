// Browse-area list pages — VendorsList, PeopleList, AssetsList,
// AssetsList, CatalogBrowser. All five share the same shape:
// filter sidebar (search + 1-3 filter button groups) + a list/
// table that's either rows or cards. Tests assert filter
// behaviour + row navigation, skipping data-dependent checks
// when the audit_log seed is missing the relevant events.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Vendors list (/vendors) — controls', () => {
  test('page mounts with filter sidebar', async ({ page }) => {
    await mountPage(page, '/vendors', { titleMatch: /vendors/i });
    await expect(page.locator('aside.catalog-filters')).toBeVisible({
      timeout: 10_000,
    });
  });

  test('Category filter — All button toggles active', async ({ page }) => {
    await mountPage(page, '/vendors', { titleMatch: /vendors/i });
    const allBtn = page
      .locator('aside.catalog-filters .filter-group')
      .nth(0)
      .getByRole('button', { name: /^all/i })
      .first();
    if ((await allBtn.count()) === 0) {
      test.skip(true, 'no vendor category buttons rendered');
    }
    await allBtn.click();
    await expect(allBtn).toHaveClass(/filter-btn-active/);
  });

  test('row click navigates to /vendors/{id}', async ({ page }) => {
    await mountPage(page, '/vendors', { titleMatch: /vendors/i });
    const link = page.locator('a[href*="/vendors/"]').first();
    if ((await link.count()) === 0) {
      test.skip(true, 'no vendor rows in seed');
    }
    await Promise.all([
      page.waitForURL(/\/vendors\/[^/]+/, { timeout: 10_000 }),
      link.click(),
    ]);
  });
});

test.describe('People list (/people) — controls', () => {
  test('View mode buttons (List / Hierarchy) toggle active', async ({ page }) => {
    await mountPage(page, '/people', { titleMatch: /people|employees/i });
    const list = page.getByRole('button', { name: /^list$/i });
    const tree = page.getByRole('button', { name: /^hierarchy$/i });
    if ((await list.count()) === 0) {
      test.skip(true, 'view-mode buttons not rendered');
    }
    await tree.click();
    await expect(tree).toHaveClass(/filter-btn-active/);
    await list.click();
    await expect(list).toHaveClass(/filter-btn-active/);
  });

  test('Status filter buttons toggle active', async ({ page }) => {
    await mountPage(page, '/people', { titleMatch: /people|employees/i });
    for (const label of ['active', 'on-leave', 'all']) {
      const btn = page
        .getByRole('button', { name: new RegExp(`^${label}\\b`, 'i') })
        .first();
      if ((await btn.count()) === 0) continue;
      await btn.click();
      await expect(btn).toHaveClass(/filter-btn-active/);
    }
  });
});

test.describe('Assets list (/assets) — controls', () => {
  test('Phase filter — All button toggles active', async ({ page }) => {
    await mountPage(page, '/assets');
    const allBtn = page
      .locator('aside.catalog-filters button')
      .filter({ hasText: /\ball\b/i })
      .first();
    await expect(allBtn).toBeVisible({ timeout: 10_000 });
    await allBtn.click();
    await expect(allBtn).toHaveClass(/filter-button-active/);
  });

  test('Search input persists user input', async ({ page }) => {
    await mountPage(page, '/assets');
    const search = page.locator('aside.catalog-filters input').first();
    await search.fill('SN-12345');
    await expect(search).toHaveValue('SN-12345');
  });

  test('row link navigates to /assets/{serial}', async ({ page }) => {
    await mountPage(page, '/assets');
    const link = page.locator('a[href*="/assets/"]').first();
    if ((await link.count()) === 0) {
      test.skip(true, 'no asset rows in seed');
    }
    await Promise.all([
      page.waitForURL(/\/assets\/[^/]+/, { timeout: 10_000 }),
      link.click(),
    ]);
  });
});

test.describe('Marketing assets list (/marketing-assets) — controls', () => {
  test('Kind filter buttons toggle active state', async ({ page }) => {
    await mountPage(page, '/marketing-assets', { titleMatch: /marketing assets/i });
    const buttons = page
      .locator('aside.catalog-filters button')
      .filter({ hasText: /./ });
    const count = await buttons.count();
    if (count === 0) {
      test.skip(true, 'no asset filter buttons rendered');
    }
    await buttons.first().click();
    await expect(buttons.first()).toHaveClass(/filter-btn-active/);
  });

  test('Include retired toggle', async ({ page }) => {
    await mountPage(page, '/marketing-assets', { titleMatch: /marketing assets/i });
    const retired = page.getByRole('button', { name: /retired/i }).first();
    if ((await retired.count()) === 0) {
      test.skip(true, 'no retired toggle');
    }
    await retired.click();
    await expect(retired).toHaveClass(/filter-btn-active/);
  });
});

test.describe('Catalog browser (/catalog) — controls', () => {
  test('Category filter — All button toggles active', async ({ page }) => {
    await mountPage(page, '/catalog', { titleMatch: /systems|catalog/i });
    const allBtn = page
      .locator('aside.catalog-filters button')
      .filter({ hasText: /^all$/i })
      .first();
    if ((await allBtn.count()) === 0) {
      test.skip(true, 'catalog empty (no models)');
    }
    await allBtn.click();
    await expect(allBtn).toHaveClass(/filter-btn-active/);
  });

  test('clicking a catalog card navigates to /catalog/{sku}', async ({
    page,
  }) => {
    await mountPage(page, '/catalog', { titleMatch: /systems|catalog/i });
    const card = page.locator('.catalog-card').first();
    if ((await card.count()) === 0) {
      test.skip(true, 'catalog empty');
    }
    await Promise.all([
      page.waitForURL(/\/catalog\/[^/]+/, { timeout: 10_000 }),
      card.click(),
    ]);
  });
});
