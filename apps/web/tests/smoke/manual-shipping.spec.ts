// Manual (/manual) — collapsible nav tree. Section nodes have a
// toggle (▸/▾) + a label link. Shipping (/shipping) — direction
// tabs (All / Inbound / Outbound) + status filter buttons.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Manual (/manual) — controls', () => {
  test('tree renders with at least one node', async ({ page }) => {
    await mountPage(page, '/manual', { titleMatch: /company manual/i });
    await expect(page.locator('.manual-tree')).toBeVisible({ timeout: 10_000 });
    const nodes = page.locator('.manual-tree-node');
    expect(await nodes.count()).toBeGreaterThan(0);
  });

  test('node label link navigates to /manual/{slug}', async ({ page }) => {
    await mountPage(page, '/manual', { titleMatch: /company manual/i });
    const link = page.locator('.manual-tree-label').first();
    if ((await link.count()) === 0) {
      test.skip(true, 'manual empty');
    }
    await Promise.all([
      page.waitForURL(/\/manual\/[^/]+/, { timeout: 10_000 }),
      link.click(),
    ]);
  });

  test('toggle button is clickable on tree nodes', async ({ page }) => {
    await mountPage(page, '/manual', { titleMatch: /company manual/i });
    // Toggles with children carry an aria-label of "Expand" or
    // "Collapse"; childless ones don't (and clicking is a no-op).
    const collapseable = page.locator('.manual-tree-toggle[aria-label]');
    if ((await collapseable.count()) === 0) {
      test.skip(true, 'no collapsable nodes in tree');
    }
    await collapseable.first().click();
    await expect(page.locator('.manual-tree')).toBeVisible();
  });
});

test.describe('Shipping (/shipping) — controls', () => {
  test('direction tabs toggle aria-selected', async ({ page }) => {
    await mountPage(page, '/shipping');
    for (const label of ['All', 'Inbound', 'Outbound']) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
      await expect(tab).toHaveClass(/tab-active/);
    }
  });

  test('Search input persists user input', async ({ page }) => {
    await mountPage(page, '/shipping');
    const search = page.locator('aside.catalog-filters input').first();
    await search.fill('TRACK-123');
    await expect(search).toHaveValue('TRACK-123');
  });

  test('Status filter — All button toggles active', async ({ page }) => {
    await mountPage(page, '/shipping');
    // The "All" status button is always rendered; per-status
    // buttons only render when there are shipments in that
    // status, so we keep the test minimal.
    const allBtn = page
      .locator('aside.catalog-filters button')
      .filter({ hasText: /^all/i })
      .first();
    if ((await allBtn.count()) === 0) {
      test.skip(true, 'shipping page has no filter buttons in this seed');
    }
    await allBtn.click();
    await expect(allBtn).toHaveClass(/filter-btn-active/);
  });
});
