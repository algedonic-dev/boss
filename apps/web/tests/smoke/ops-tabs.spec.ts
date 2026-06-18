// Operations area — Warehouse, HR, Schedule. All three pivot on
// tab navigation; each tab also has its own filter sidebar / row
// controls. This spec covers the tab-switching + week-pagination
// + the in-tab filter button toggle.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Warehouse (/warehouse) — controls', () => {
  test('tab buttons toggle aria-selected + active class', async ({ page }) => {
    await mountPage(page, '/warehouse');
    for (const label of ['Overview', 'Inventory', 'Receiving']) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
      await expect(tab).toHaveClass(/tab-active/);
    }
  });

  test('Inventory tab shows the inventory filter buttons', async ({ page }) => {
    await mountPage(page, '/warehouse');
    await page.getByRole('tab', { name: 'Inventory' }).click();
    // Three FilterButtons exposed: All / Critical / Low.
    for (const label of ['critical', 'low', 'all']) {
      const btn = page.getByRole('button', { name: new RegExp(label, 'i') });
      await btn.first().click();
      await expect(btn.first()).toHaveClass(/filter-btn-active/);
    }
  });

  test('Receiving tab toggles the Create PO form', async ({ page }) => {
    await mountPage(page, '/warehouse');
    await page.getByRole('tab', { name: 'Receiving' }).click();
    const createBtn = page.getByRole('button', { name: /create po|create purchase order/i });
    if ((await createBtn.count()) === 0) {
      test.skip(true, 'Receiving tab has no create-PO button (role-gated)');
    }
    // Toggling the create-PO panel — clicking shows / hides
    // additional form controls.
    await createBtn.first().click();
    // Submit button only renders inside the create-PO panel.
    await expect(
      page.getByRole('button', { name: /^create$|submit/i }).first(),
    ).toBeVisible({ timeout: 5_000 });
  });
});

test.describe('HR (/hr) — controls', () => {
  test('tab buttons toggle aria-selected', async ({ page }) => {
    await mountPage(page, '/hr');
    for (const label of [
      'Overview',
      'Workflows',
      'Requisitions',
      'Certifications',
      'Headcount',
    ]) {
      const tab = page.getByRole('tab', { name: label });
      await tab.click();
      await expect(tab).toHaveAttribute('aria-selected', 'true', { timeout: 3_000 });
    }
  });
});

test.describe('Schedule (/service/schedule) — week navigation', () => {
  test('Prev / This / Next week buttons navigate', async ({ page }) => {
    await mountPage(page, '/service/schedule');
    const prev = page.getByRole('button', { name: /prev week|← prev|previous/i });
    const next = page.getByRole('button', { name: /next week|next →/i });
    const today = page.getByRole('button', { name: /this week/i });
    await expect(prev).toBeVisible({ timeout: 10_000 });
    await expect(today).toBeVisible();
    await expect(next).toBeVisible();

    // Round-trip: capture the page header subtitle (carries the
    // week range), shift forward, shift back, assert reset.
    const headerBefore = await page.locator('h1').first().textContent();
    await next.click();
    await page.waitForTimeout(200);
    await prev.click();
    await page.waitForTimeout(200);
    await expect(page.locator('h1').first()).toHaveText(headerBefore!, {
      timeout: 5_000,
    });
  });
});
