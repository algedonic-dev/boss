import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { BREWERY_PRODUCTS } from '../../src/shop/brewery-products';

const PRODUCT = BREWERY_PRODUCTS[0]!;
const ORDER = 'shop-order';
type Failure = 'shipment' | 'billing' | 'both' | 'read' | 'parse' | 'network' | 'none';
type Write = Readonly<{ method: string; path: string; body: unknown }>;

async function checkout(page: Page, failure: Failure): Promise<Write[]> {
  const writes: Write[] = [];
  await installSmokeMocks(page);
  await page.route('**/api/jobs/step-types', (route) => route.fulfill({
    json: [{ kind: 'ship-order', surface: 'shipment' }, { kind: 'bill-order', surface: 'billing' }],
  }));
  await page.route('**/api/inventory/items', (route) => route.fulfill({
    json: [{ part_sku: PRODUCT.sku, on_hand: 10, allocated: 0 }],
  }));
  await page.route('**/api/customers', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    writes.push({ method: 'POST', path: '/api/customers', body: route.request().postDataJSON() });
    await route.fulfill({ json: { id: 'buyer' } });
  });
  await page.route('**/api/jobs', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    writes.push({ method: 'POST', path: '/api/jobs', body: route.request().postDataJSON() });
    await route.fulfill({ json: { id: ORDER } });
  });
  await page.route(`**/api/jobs/${ORDER}/steps`, async (route) => {
    if (failure === 'network') return route.abort('failed');
    if (failure === 'read') return route.fulfill({ status: 403, body: 'order details denied' });
    if (failure === 'parse') return route.fulfill({ contentType: 'application/json', body: '{' });
    await route.fulfill({ json: [{ id: 'shipment', kind: 'ship-order' }, { id: 'billing', kind: 'bill-order' }] });
  });
  await page.route(`**/api/jobs/${ORDER}/steps/*/metadata`, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const target = path.includes('/shipment/') ? 'shipment' : 'billing';
    writes.push({ method: route.request().method(), path, body: route.request().postDataJSON() });
    if (failure === target || failure === 'both') {
      return route.fulfill({ status: target === 'shipment' ? 403 : 409, json: { error: `${target} details refused` } });
    }
    await route.fulfill({ json: {} });
  });
  await mountPage(page, `/ux/shop/${PRODUCT.sku}`);
  await expect(page.locator('.shop-stock-in')).toBeVisible();
  await page.getByLabel('Your name', { exact: true }).fill('Sam Brewer');
  await page.getByLabel('Email', { exact: true }).fill('sam@example.test');
  await page.getByRole('button', { name: 'Place order', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Order placed', exact: true })).toBeVisible();
  return writes;
}

for (const failure of ['shipment', 'billing', 'both', 'read', 'parse', 'network'] as const) {
  test(`checkout keeps the order and shows refused ${failure} details`, async ({ page }) => {
    const writes = await checkout(page, failure);
    const alerts = page.locator('.shop-quote-form [role="alert"]');
    await expect(alerts).toHaveCount(failure === 'both' ? 2 : 1);
    await expect(alerts.first()).toContainText('Order saved');
    if (failure === 'shipment' || failure === 'both') await expect(alerts).toContainText(failure === 'both'
      ? ['shipment details refused', 'billing details refused'] : ['shipment details refused']);
    if (failure === 'billing') await expect(alerts).toContainText(['billing details refused']);
    await expect(page.locator('.shop-quote-success')).toContainText(ORDER);
    await expect(page.getByRole('button', { name: 'Place order', exact: true })).toHaveCount(0);
    expect(writes.filter((w) => w.path === '/api/jobs')).toHaveLength(1);
    if (['shipment', 'billing', 'both'].includes(failure)) {
      await expect.poll(() => writes.filter((w) => w.method === 'PATCH').length).toBe(2);
      expect(writes.filter((w) => w.method === 'PATCH').map((w) => w.path)).toEqual([
        `/api/jobs/${ORDER}/steps/shipment/metadata`, `/api/jobs/${ORDER}/steps/billing/metadata`,
      ]);
    } else expect(writes.filter((w) => w.method === 'PATCH')).toEqual([]);
  });
}

test('checkout saves both detail overlays without an alert or another order', async ({ page }) => {
  const writes = await checkout(page, 'none');
  await expect.poll(() => writes.filter((w) => w.method === 'PATCH').length).toBe(2);
  await expect(page.locator('.shop-quote-form [role="alert"]')).toHaveCount(0);
  expect(writes.filter((w) => w.path === '/api/jobs')).toHaveLength(1);
  const patches = writes.filter((w) => w.method === 'PATCH');
  expect(patches[0]?.body).toMatchObject({ line_items: [{ part_sku: PRODUCT.sku, qty: 1 }], consumes_products: [{ sku: PRODUCT.sku, qty: 1 }] });
  expect(patches[1]?.body).toMatchObject({ line_items: [{ part_sku: PRODUCT.sku, qty: 1 }], amount_cents: PRODUCT.unit_price_cents });
});
