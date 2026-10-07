import { isShellWrite } from './_smokeMocks';
// Whole root-page controls for page-audit a98b6d6a. Checkout is a
// separate route and obligation; these navigation checks never order.
import { expect, test, type Page } from './_test';
import { answerRead, mountPage, openedRequests, recordPageRequests } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE, LIVE_MANIFEST_RECORDED_AT } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { BREWERY_PRODUCTS, packageLabel, priceLabel } from '../../src/shop/brewery-products';
import { parseRoute } from '../../src/router';
import { HOME_APP } from '@boss/web-kit/nav';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = '/ux/shop';
const ITEMS = '/api/inventory/items';
const cards = (page: Page) => page.locator('article.shop-card');
const available = (page: Page) => cards(page).locator('.shop-card-avail');

async function setup(page: Page, body: unknown): Promise<void> {
  await installSmokeMocks(page);
  await recordPageRequests(page);
  await page.route(`**${ITEMS}`, (r) => r.fulfill({
    status: 200, contentType: 'application/json', body: JSON.stringify(body),
  }));
}

async function root(page: Page): Promise<void> {
  await mountPage(page, PATH);
  await expect(page.getByRole('heading', { name: 'Algedonic Ales — Storefront' })).toBeVisible();
  await answerRead(page, /^\/api\/inventory\/items$/);
}

async function noWrites(page: Page): Promise<void> {
  const writes = (await openedRequests(page)).filter((r) => r.method !== 'GET');
  // App's shared route-open telemetry is a real write, separate from
  // Shop's zero business writes. Refuse every other path and method.
  expect(writes.every((r) => isShellWrite(r.method, r.path))).toBe(true);
}

test.describe('/ux/shop — whole root controls', () => {
  test('every catalog card keeps its words, five buttons and five detail links', async ({ page }) => {
    await setup(page, []);
    await root(page);
    expect(ROUTE_CATALOG.shop).toMatchObject({ path: PATH, module: 'shop', owner: 'sales' });
    expect(parseRoute(PATH)).toEqual({ kind: 'shop' });
    await expect(page.locator('.shop-hero-sub')).toHaveText("Pick up beer direct from the brewery. Kegs ship on local routes; bottles ship via packing slip. Stock numbers come straight off the warehouse projection — what you see is what's in the cooler right now.");
    await expect(cards(page)).toHaveCount(BREWERY_PRODUCTS.length);
    await expect(cards(page).getByRole('button')).toHaveCount(5);
    await expect(cards(page).getByRole('link', { name: 'View details', exact: true })).toHaveCount(5);
    await expect(page.locator('.catalog form')).toHaveCount(0);
    for (const [i, product] of BREWERY_PRODUCTS.entries()) {
      const card = cards(page).nth(i);
      await expect(card.getByRole('heading')).toHaveText(product.brand);
      await expect(card.locator('.shop-card-category')).toHaveText(product.style);
      await expect(card.locator('.shop-card-tagline')).toHaveText(product.tagline);
      await expect(card.locator('.shop-card-specs .chip')).toHaveText([
        `${product.abv_pct}% ABV`, `${product.ibu} IBU`, packageLabel(product.package),
      ]);
      await expect(card.locator('.shop-card-price-value')).toHaveText(priceLabel(product.unit_price_cents));
      await expect(card.locator('.shop-card-limited')).toHaveCount(product.available_until === null ? 0 : 1);
      if (product.available_until !== null) await expect(card.locator('.shop-card-limited')).toHaveText('Limited');
    }
    await expect(available(page)).toHaveText(BREWERY_PRODUCTS.map(() => 'check availability'));
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    expect((await openedRequests(page)).filter((r) => r.path === ITEMS)).toHaveLength(1);
    await noWrites(page);
  });

  for (const activation of ['link', 'button', 'Enter', 'Space'] as const) {
    test(`all five products activate through ${activation} and browser Back returns`, async ({ page }) => {
      await setup(page, []);
      await root(page);
      for (const [i, product] of BREWERY_PRODUCTS.entries()) {
        const destination = `${PATH}/${encodeURIComponent(product.sku)}`;
        // The router resolves product children under the catalogued Shop
        // route; a second fixture list of allowed destinations would drift.
        expect(parseRoute(destination)).toEqual({ kind: 'shopProduct', sku: product.sku });
        const card = cards(page).nth(i);
        const link = card.getByRole('link', { name: 'View details', exact: true });
        await expect(link).toHaveAttribute('href', destination);
        const button = card.getByRole('button', { name: `View details for ${product.brand} (${packageLabel(product.package)})`, exact: true });
        if (activation === 'link') await link.click();
        else if (activation === 'button') await button.click();
        else {
          await button.focus();
          await button.press(activation);
        }
        await expect(page).toHaveURL(new RegExp(`${destination}$`));
        await expect(page.locator('h1.detail-title')).toHaveText(product.brand);
        await expect(page.locator('.detail-page a.breadcrumb')).toHaveAttribute('href', PATH);
        await page.goBack();
        await expect(page).toHaveURL(new RegExp(`${PATH}$`));
        await expect(cards(page)).toHaveCount(5);
      }
      await noWrites(page);
    });
  }

  test('stock means unallocated units, floors zero, and leaves missing SKUs unknown', async ({ page }) => {
    const sku = (index: number): string => {
      const product = BREWERY_PRODUCTS[index];
      if (!product) throw new Error(`missing catalog product ${index}`);
      return product.sku;
    };
    await setup(page, { data: [
      { part_sku: sku(0), on_hand: 20, allocated: 2 },
      { part_sku: sku(1), on_hand: 8, allocated: 2 },
      { part_sku: sku(2), on_hand: 1, allocated: 0 },
      { part_sku: sku(3), on_hand: 1, allocated: 2 },
      { part_sku: 'RAW-MALT', on_hand: 999, allocated: 0 },
    ] });
    await root(page);
    await expect(available(page)).toHaveText(['18 in stock', 'only 6 left', 'only 1 left', 'sold out', 'check availability']);
    await expect(available(page)).toHaveClass([
      /shop-card-avail-in/, /shop-card-avail-low/, /shop-card-avail-low/, /shop-card-avail-out/, /shop-card-avail-unknown/,
    ]);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    await noWrites(page);
  });

  for (const [label, body] of [['bare list', []], ['envelope', { data: [] }]] as const) {
    test(`a genuinely empty ${label} leaves availability unknown without claiming outage or sold out`, async ({ page }) => {
      await setup(page, body);
      await root(page);
      await expect(available(page)).toHaveText(BREWERY_PRODUCTS.map(() => 'check availability'));
      await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
      await noWrites(page);
    });
  }

  for (const [label, body] of [['object', {}], ['non-list data', { data: 'bad' }], ['null', null], ['string', 'bad']] as const) {
    test(`a malformed successful ${label} is a named failure, never healthy empty stock`, async ({ page }) => {
      await setup(page, body);
      await root(page);
      await expect(page.locator(FAILURE_MARKER)).toContainText("Couldn't load stock levels — /api/inventory/items: HTTP 200, but");
      await expect(page.locator(FAILURE_MARKER)).toContainText('not a list or a {data: [...]} envelope');
      await expect(page.locator(FAILURE_MARKER)).toHaveAttribute('role', 'alert');
      await expect(available(page)).toHaveText(BREWERY_PRODUCTS.map(() => 'check availability'));
      await noWrites(page);
    });
  }

  test('a refused inventory read keeps all ten navigation controls working without making a stock or order claim', async ({ page }) => {
    await setup(page, []);
    await page.route(`**${ITEMS}`, (r) => r.fulfill({ status: 503, body: 'inventory unavailable' }));
    await mountPage(page, PATH);
    const failed = "Couldn't load stock levels — /api/inventory/items: HTTP 503. Availability below is unknown, not sold out.";
    await expect(page.locator(FAILURE_MARKER)).toHaveText(failed);
    for (const activation of ['link', 'button'] as const) {
      for (const [i, product] of BREWERY_PRODUCTS.entries()) {
        await expect(available(page)).toHaveText(BREWERY_PRODUCTS.map(() => 'check availability'));
        const card = cards(page).nth(i);
        await (activation === 'link' ? card.getByRole('link') : card.getByRole('button')).click();
        await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/${product.sku}`);
        await expect(page.locator('h1.detail-title')).toHaveText(product.brand);
        await page.goBack();
        await expect(page.locator(FAILURE_MARKER)).toHaveText(failed);
      }
    }
    await noWrites(page);
  });

  test(`current tenant recording ${LIVE_MANIFEST_RECORDED_AT}: module off has exact home and Back, no inventory read or write`, async ({ page }) => {
    expect(MODULES_LIVE.shop).toBe(false);
    await setup(page, []);
    await installTenantManifest(page, MODULES_LIVE, { inline: true });
    await mountPage(page, PATH);
    await expect(page.getByRole('heading', { name: 'Not enabled for this tenant' })).toBeVisible();
    await expect(cards(page)).toHaveCount(0);
    expect(parseRoute(HOME_APP.href)).toEqual({ kind: 'me' });
    await page.getByRole('button', { name: 'Back to home', exact: true }).click();
    await expect.poll(() => new URL(page.url()).pathname).toBe(HOME_APP.href);
    await expect(page.getByText('Not signed in. Sign in to see your day.', { exact: true })).toBeVisible();
    await page.goBack();
    await expect(page).toHaveURL(new RegExp(`${PATH}$`));
    await expect(page.getByRole('heading', { name: 'Not enabled for this tenant' })).toBeVisible();
    expect((await openedRequests(page)).filter((r) => r.path === ITEMS)).toEqual([]);
    await noWrites(page);
  });
});
