// Render-contract coverage for routes that previously had no
// dedicated smoke spec. Each test is the same shape as
// detail-pages.spec.ts: navigate to the route, assert the
// AppShell paints, assert an h1 paints. The contract here is
// "this route renders without crashing" — interaction tests
// belong in per-page specs.
//
// Filed 2026-05-04 to close the partial-SPA-coverage gap. The
// 14-day regen produces enough seed data that all of these
// routes have something to render; tests skip cleanly when the
// brewery seed doesn't exercise a particular surface (e.g. used-
// device-shop /refurb against the brewery tenant).

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

test.describe('Page-coverage gaps — render contract', () => {
  test('ProductsList (/products) mounts', async ({ page }) => {
    await mountPage(page, '/products');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('ProductPage (/products/{sku}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/products');
    const link = page.locator('a[href*="/products/"]').first();
    if ((await link.count()) === 0) {
      test.skip(true, 'no products in seed');
    }
    await Promise.all([
      page.waitForURL(/\/products\/[^/]+/, { timeout: 10_000 }),
      link.click(),
    ]);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('ItKnowledgeBasePage (/it/kb) mounts', async ({ page }) => {
    await mountPage(page, '/it/kb');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('IT monitoring root (/it/monitoring) mounts', async ({ page }) => {
    await mountPage(page, '/it/monitoring');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('IT monitoring perf (/it/monitoring/perf) mounts', async ({ page }) => {
    await mountPage(page, '/it/monitoring/perf');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('IT monitoring atlas (/it/monitoring/atlas) mounts', async ({
    page,
  }) => {
    await mountPage(page, '/it/monitoring/atlas');
    // Atlas is an SVG-driven view; either an h1 (header strip) or
    // the atlas SVG itself should paint. Accept either.
    const headerOrSvg = page
      .locator('h1, svg.atlas, .atlas svg')
      .first();
    await expect(headerOrSvg).toBeVisible({ timeout: 10_000 });
  });

  test('Policy (/policy) mounts', async ({ page }) => {
    await mountPage(page, '/policy');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('Step Plugins admin (/admin/step-plugins) mounts', async ({ page }) => {
    await mountPage(page, '/admin/step-plugins');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('Refurb dashboard (/refurb) mounts', async ({ page }) => {
    // Tenant-specific surface — used-device-shop. Skip cleanly on
    // brewery-only deploys where the route is mounted but renders
    // an empty state.
    await mountPage(page, '/refurb');
    const visible = page.locator('h1').first();
    await expect(visible).toBeVisible({ timeout: 10_000 });
  });

  test('Sales home (/sales) mounts', async ({ page }) => {
    await mountPage(page, '/sales');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('Service home (/service) mounts', async ({ page }) => {
    await mountPage(page, '/service');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('Auth admin (/auth-admin) mounts', async ({ page }) => {
    // Auth admin is a thin per-tenant configuration page; in demo
    // mode it renders a status panel.
    await mountPage(page, '/auth-admin');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });
});
