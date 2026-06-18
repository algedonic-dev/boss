// Detail-page mount checks. These are surfaces with few or no
// controls of their own — the contract being tested is "the
// route renders without crashing when given a valid id."
//
// Each test discovers a real id from its corresponding list
// page (so we don't bake brittle ids into the spec), then
// navigates and asserts the AppShell + h1 paint. Skip cleanly
// when the seed has no rows for that resource.

import { test, expect } from '@playwright/test';
import { mountPage } from './_helpers';

async function firstHrefMatching(
  page: import('@playwright/test').Page,
  pattern: RegExp,
): Promise<string | null> {
  const links = page.locator(`a[href*="${pattern.source.replace(/\\\//g, '/').split('[')[0]}"]`);
  const n = await links.count();
  for (let i = 0; i < n; i++) {
    const href = await links.nth(i).getAttribute('href');
    if (href && pattern.test(href)) return href;
  }
  return null;
}

test.describe('Detail pages — render contract', () => {
  test('AccountPage (/accounts/{id}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/accounts');
    const link = await firstHrefMatching(page, /\/accounts\/[^/]+/);
    if (!link) test.skip(true, 'no accounts seeded');
    await page.goto(link!);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('VendorPage (/vendors/{id}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/vendors');
    const link = await firstHrefMatching(page, /\/vendors\/[^/]+/);
    if (!link) test.skip(true, 'no vendors seeded');
    await page.goto(link!);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('EmployeePage (/people/{id}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/people');
    const link = await firstHrefMatching(page, /\/people\/[^/]+/);
    if (!link) test.skip(true, 'no employees seeded');
    await page.goto(link!);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('PartPage (/parts/{sku}) mounts when seeded', async ({ page }) => {
    // Use a known-good SKU from the brewery inventory seed.
    await page.goto('/parts/ING-HOPS-CASCADE-44');
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('AssetPage (/assets/{serial}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/assets');
    const link = await firstHrefMatching(page, /\/assets\/[^/]+/);
    if (!link) test.skip(true, 'no assets seeded');
    await page.goto(link!);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('AssetPage (/assets/{id}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/assets');
    const link = await firstHrefMatching(page, /\/assets\/[^/]+/);
    if (!link) test.skip(true, 'no assets seeded');
    await page.goto(link!);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('DevicePage (/catalog/{sku}) mounts when seeded', async ({ page }) => {
    await mountPage(page, '/catalog');
    const link = await firstHrefMatching(page, /\/catalog\/[^/]+/);
    if (!link) test.skip(true, 'no catalog models seeded');
    await page.goto(link!);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('ShipmentPage (/shipments/{id}) mounts when seeded', async ({ page }) => {
    // Brewery shipping rebuild produces ship-step-{step_id} ids.
    // Pick the first one off /api/shipping/shipments via a fetch
    // inside the page context to avoid baking an id into the spec.
    await page.goto('/');
    const id = await page.evaluate(async () => {
      const r = await fetch('/api/shipping/shipments?limit=1');
      if (!r.ok) return null;
      const body = await r.json();
      const arr = Array.isArray(body) ? body : (body.data ?? []);
      return arr[0]?.id ?? null;
    });
    if (!id) test.skip(true, 'no shipments seeded');
    await page.goto(`/shipments/${id}`);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('InvoicePage (/finance/{id}) mounts when seeded', async ({ page }) => {
    await page.goto('/');
    const id = await page.evaluate(async () => {
      const r = await fetch('/api/commerce/invoices?limit=1');
      if (!r.ok) return null;
      const body = await r.json();
      const arr = Array.isArray(body) ? body : (body.data ?? []);
      return arr[0]?.id ?? null;
    });
    if (!id) test.skip(true, 'no invoices seeded');
    await page.goto(`/finance/${encodeURIComponent(id)}`);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('JobDetailPage (/jobs/{id}) mounts when seeded', async ({ page }) => {
    await page.goto('/');
    const id = await page.evaluate(async () => {
      const r = await fetch('/api/jobs?limit=1');
      if (!r.ok) return null;
      const body = await r.json();
      const arr = Array.isArray(body) ? body : (body.data ?? []);
      return arr[0]?.id ?? null;
    });
    if (!id) test.skip(true, 'no jobs seeded');
    await page.goto(`/jobs/${id}`);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });

  test('PoPage (/purchase-orders/{id}) mounts when seeded', async ({ page }) => {
    await page.goto('/');
    const id = await page.evaluate(async () => {
      const r = await fetch('/api/inventory/purchase-orders?limit=1');
      if (!r.ok) return null;
      const body = await r.json();
      const arr = Array.isArray(body) ? body : (body.data ?? []);
      return arr[0]?.id ?? null;
    });
    if (!id) test.skip(true, 'no purchase orders seeded');
    await page.goto(`/purchase-orders/${encodeURIComponent(id)}`);
    await expect(page.locator('.app-shell')).toBeVisible({ timeout: 10_000 });
    await expect(page.locator('h1').first()).toBeVisible();
  });
});

test.describe('Other surfaces — render contract', () => {
  test('ArchitecturePage (/architecture) mounts', async ({ page }) => {
    await mountPage(page, '/architecture');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });

  test('OpsDashboard (/ops) mounts', async ({ page }) => {
    await mountPage(page, '/ops');
    await expect(page.locator('h1').first()).toBeVisible({ timeout: 10_000 });
  });
});
