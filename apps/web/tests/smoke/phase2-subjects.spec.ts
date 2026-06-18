// Phase-2 Playwright smoke — the Browse bucket (accounts, vendors,
// people, parts, shipping, support). Mirrors the shape of
// primitives.spec.ts: lightweight checks that each list + detail
// route renders without blowing up and carries at least one row
// against the seeded DB.

import { test, expect } from '@playwright/test';

test.describe('Phase-2 Browse subjects', () => {
  test('accounts list renders rows', async ({ page }) => {
    await page.goto('/accounts');
    await expect(page.locator('h1')).toContainText(/accounts/i);
    await expect(page.locator('table tbody tr').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('account detail answers the five load-bearing questions', async ({ page }) => {
    await page.goto('/accounts/account-00001');
    await expect(page.locator('h1')).toBeVisible();
    await expect(page.locator('h1')).not.toHaveText(/Not found/i);
    await expect(page.locator('.pp-glance-stat')).toHaveCount(5, { timeout: 10_000 });
    await expect(
      page.locator('section').filter({ hasText: /Activity timeline/i }),
    ).toBeVisible();
    await expect(page.locator('.pp-tabs')).toBeVisible();
  });

  test('account knowledge tab renders the KB view', async ({ page }) => {
    await page.goto('/accounts/account-00001');
    await expect(page.locator('.pp-tabs')).toBeVisible({ timeout: 10_000 });
    await page.locator('.pp-tab', { hasText: /Knowledge/ }).click();
    // KB view always renders a Timeline section + an Insights
    // placeholder, even when the account has zero facts / jobs /
    // documents — those two are our proof the KB component mounted.
    await expect(
      page.locator('.kb-view section').filter({ hasText: /Timeline/ }),
    ).toBeVisible({ timeout: 10_000 });
    await expect(
      page.locator('.kb-view section').filter({ hasText: /Insights/ }),
    ).toBeVisible();
  });

  test('vendors list renders rows', async ({ page }) => {
    await page.goto('/vendors');
    await expect(page.locator('h1')).toContainText(/vendors/i);
    const panel = page.locator('.list-section');
    await expect(
      panel.locator('table, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('people list renders rows', async ({ page }) => {
    await page.goto('/people');
    await expect(page.locator('h1')).toContainText(/employees/i);
    await expect(page.locator('table tbody tr').first()).toBeVisible({
      timeout: 10_000,
    });
  });

  test('employee detail loads for a real id', async ({ page }) => {
    await page.goto('/people');
    const firstRow = page.locator('table tbody tr').first();
    await expect(firstRow).toBeVisible({ timeout: 10_000 });
    const link = firstRow.locator('a').first();
    const href = await link.getAttribute('href');
    expect(href).toMatch(/\/people\//);
    await page.goto(href!);
    await expect(page.locator('section').filter({ hasText: /Profile/ })).toBeVisible({
      timeout: 10_000,
    });
  });

  test('parts list renders', async ({ page }) => {
    await page.goto('/parts');
    await expect(page.locator('h1')).toContainText(/parts/i);
    const panel = page.locator('.list-section');
    await expect(
      panel.locator('table, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('shipping dashboard renders', async ({ page }) => {
    await page.goto('/shipping');
    await expect(page.locator('h1')).toContainText(/shipments/i);
    const panel = page.locator('.list-section');
    await expect(
      panel.locator('table, p.empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('support dashboard renders tabs', async ({ page }) => {
    await page.goto('/support');
    await expect(page.locator('h1')).toContainText(/open cases/i);
    await expect(page.locator('.tabs')).toBeVisible();
  });
});
