import { expect, test } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, isPageWrite } from './_smokeMocks';
import { ROUTES } from './_routes';
import { CRAWL_VENDOR, VENDOR_CRAWL_PATH } from '../fixtures/vendor';

test('the shared crawls have a seeded native VendorPage representative', () => {
  expect(ROUTES).toContain('/ux/vendors');
  expect(ROUTES.some((path) => /^\/ux\/vendors\/[^/]+$/.test(path))).toBe(true);
});

test('the shared smoke fixture mounts a real vendor detail, not Vendor not found', async ({ page }) => {
  await installSmokeMocks(page);
  const writes: string[] = [];
  const reads: string[] = [];
  page.on('request', (request) => {
    const path = new URL(request.url()).pathname;
    if (isPageWrite(request.method(), path)) writes.push(path);
    if (request.method() === 'GET') reads.push(path);
  });
  await mountPage(page, VENDOR_CRAWL_PATH);
  await expect(page.locator('h1.detail-title')).toHaveText(CRAWL_VENDOR.name!);
  await expect(page.getByText('Vendor not found', { exact: true })).toHaveCount(0);
  for (const suffix of ['contacts', 'interactions', 'account-team', 'contracts']) {
    expect(reads).toContain(`/api/inventory/vendors/${CRAWL_VENDOR.id}/${suffix}`);
  }
  expect(reads).toEqual(expect.arrayContaining([
    '/api/inventory/vendors', '/api/inventory/orders', '/api/inventory/vendor-invoices',
  ]));
  expect(writes).toEqual([]);
});

test('the shared VendorPage route names a refused vendor read', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/inventory\/vendors(\?|$)/, (route) =>
    route.fulfill({ status: 503, contentType: 'application/json', body: '{}' }));
  await mountPage(page, VENDOR_CRAWL_PATH);
  await expect(page.locator('.load-failed[role="alert"]').filter({ hasText: "Couldn't load this vendor" })).toBeVisible();
  await expect(page.getByText('Vendor not found', { exact: true })).toHaveCount(0);
});

test('an answered empty vendor list remains a missing detail', async ({ page }) => {
  await installSmokeMocks(page);
  await page.route(/\/api\/inventory\/vendors(\?|$)/, (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: '[]' }));
  await mountPage(page, VENDOR_CRAWL_PATH);
  await expect(page.getByText('Vendor not found', { exact: true })).toBeVisible();
  await expect(page.locator('h1.detail-title')).toHaveCount(0);
});
