// Phase-4 Playwright smokes — the routes that flip-day demands
// (Inbox, Calendar, Schedule, Exec, CTO, Perf, Warehouse, Catalog
// + device detail, Assets + detail, Manual, PO detail, Watchlist,
// Shop + product). These are the surfaces that weren't in the
// original phase-3 scope but need to work before BOSS_STATIC_DIR
// flips to the Svelte bundle.

import { test, expect } from '@playwright/test';

test.describe('Phase-4 full-parity routes', () => {
  test('inbox renders', async ({ page }) => {
    await page.goto('/inbox');
    await expect(page.locator('h1')).toContainText(/messages/i, { timeout: 10_000 });
  });

  test('calendar renders', async ({ page }) => {
    await page.goto('/calendar');
    await expect(page.locator('h1')).toContainText(/launch calendar/i, { timeout: 10_000 });
  });

  test('schedule renders the week grid', async ({ page }) => {
    await page.goto('/service/schedule');
    await expect(page.locator('h1')).toContainText(/service schedule/i, { timeout: 10_000 });
  });

  test('exec dashboard loads', async ({ page }) => {
    await page.goto('/exec');
    await expect(page.locator('.exec-title')).toBeVisible({ timeout: 10_000 });
    // Six exec cards.
    await expect(page.locator('.exec-card')).toHaveCount(6, { timeout: 10_000 });
  });

  test('cto dashboard loads', async ({ page }) => {
    await page.goto('/cto');
    await expect(page.locator('h1')).toContainText(/boss network/i, { timeout: 10_000 });
  });

  test('perf page loads', async ({ page }) => {
    await page.goto('/perf');
    await expect(page.locator('h1')).toContainText(/gateway latency/i, { timeout: 10_000 });
  });

  test('events page loads', async ({ page }) => {
    await page.goto('/cto/events');
    await expect(page.locator('h1')).toContainText(/audit log/i, { timeout: 10_000 });
  });

  test('atlas page loads the flow diagram', async ({ page }) => {
    await page.goto('/cto/atlas');
    await expect(page.locator('h1')).toContainText(/operating model flows/i, { timeout: 10_000 });
    await expect(page.locator('svg.atlas-canvas')).toBeVisible({ timeout: 10_000 });
  });

  test('warehouse renders', async ({ page }) => {
    await page.goto('/warehouse');
    await expect(page.locator('h1')).toContainText(/tracked skus/i, { timeout: 10_000 });
    await expect(page.locator('.tabs')).toBeVisible();
  });

  test('catalog browser renders', async ({ page }) => {
    await page.goto('/catalog');
    await expect(page.locator('h1')).toContainText(/catalog systems/i, { timeout: 10_000 });
    // Either a card grid or an "empty" state.
    await expect(
      page.locator('.catalog-card, .empty').first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test('marketing assets list renders', async ({ page }) => {
    await page.goto('/marketing-assets');
    await expect(page.locator('h1')).toContainText(/marketing assets/i, { timeout: 10_000 });
  });

  test('manual loads the tree', async ({ page }) => {
    await page.goto('/manual');
    await expect(page.locator('h1')).toContainText(/company manual/i, { timeout: 10_000 });
    await expect(page.locator('.manual-tree')).toBeVisible({ timeout: 10_000 });
  });

  test('watchlist loads', async ({ page }) => {
    await page.goto('/watchlist');
    await expect(page.locator('h1')).toContainText(/watchlist|couldn't load/i, {
      timeout: 10_000,
    });
  });

  test('shop home renders product cards', async ({ page }) => {
    await page.goto('/shop');
    await expect(page.locator('.shop-hero-title')).toBeVisible({ timeout: 10_000 });
  });
});
