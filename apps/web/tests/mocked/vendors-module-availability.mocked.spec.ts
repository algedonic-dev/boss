import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE, signInWithPolicy } from './_smokeMocks';

async function shape(page: Page, warehouse: boolean): Promise<void> {
  await installSmokeMocks(page);
  await signInWithPolicy(page);
  await installTenantManifest(page, { ...MODULES_LIVE, warehouse }, { inline: true });
}

for (const path of ['/ux/vendors', '/ux/vendors/vnd-review']) {
  test(`${path} is absent when warehouse is off, without inventory reads`, async ({ page }) => {
    await shape(page, false);
    const reads: string[] = [];
    page.on('request', (request) => {
      if (/\/api\/inventory\//.test(request.url())) reads.push(request.url());
    });
    await mountPage(page, path);
    await expect(page.getByRole('heading', { name: 'No page at this address', exact: true })).toBeVisible();
    await expect(page.locator('.module-disabled')).toHaveCount(0);
    await expect(page.locator('a[href="/ux/vendors"]')).toHaveCount(0);
    expect(reads).toEqual([]);
  });
}

test('warehouse-enabled tenants keep the Vendors page and navigation', async ({ page }) => {
  await shape(page, true);
  await mountPage(page, '/ux/vendors');
  await expect(page.locator('.exec-title')).toContainText('vendors');
  await expect(page.locator('a[href="/ux/vendors"]')).toHaveCount(1);
  await expect(page.getByRole('heading', { name: 'No page at this address', exact: true })).toHaveCount(0);
});

for (const [path, heading] of [
  ['/ux/purchase-orders/po-review', 'Purchase order not found'],
  ['/ux/vendor-invoices/bill-review', 'Vendor invoice not found'],
] as const) {
  test(`${path} retains its Finance detail reader with warehouse off`, async ({ page }) => {
    await shape(page, false);
    await mountPage(page, path);
    await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible();
    await expect(page.locator('.module-disabled')).toHaveCount(0);
    await expect(page.getByRole('heading', { name: 'No page at this address', exact: true })).toHaveCount(0);
  });
}

test('an unavailable manifest is unknown, never clean Vendors absence', async ({ page }) => {
  await installSmokeMocks(page);
  await signInWithPolicy(page);
  await page.addInitScript(() => {
    delete (globalThis as { __BOSS_TENANT_MANIFEST__?: unknown }).__BOSS_TENANT_MANIFEST__;
  });
  await page.route(/\/api\/tenant\/manifest$/, (route) => route.fulfill({ status: 503, json: {} }));
  await mountPage(page, '/ux/vendors');
  await expect(page.getByRole('alert')).toContainText('Module availability is unknown');
  await expect(page.getByRole('button', { name: 'Retry tenant manifest' })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'No page at this address', exact: true })).toHaveCount(0);
});
