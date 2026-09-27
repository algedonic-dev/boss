// /ux/shop's one read — backlog aaeb02d6.
//
// The storefront's catalog is a static module; its stock figures come
// from /api/inventory/items. Until this packet a refused read hit
// `if (!r.ok) return` and a rejected one a bare catch, so an inventory
// outage painted every card "check availability" — the same words a
// healthy read gives a SKU with no inventory row — and nothing on the
// page said a read had failed. The outage crawl carried the route on its
// SILENT map for exactly that. The page now keeps the read's outcome as
// a ReadState and names the failure on the shared marker.

import { expect, test, type Page, type Route } from '@playwright/test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const ITEMS = '**/api/inventory/items';

/// The first card for a brand — the catalog lists its kegs in module
/// order, so for the pale ale that is FP-PALE-1-2-BBL.
const card = (page: Page, brand: string) =>
  page.locator('article.shop-card').filter({ hasText: brand }).first();

test.describe('/ux/shop — the stock read', () => {
  test('a refused stock read names itself and says availability is unknown', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(ITEMS, (r: Route) =>
      r.fulfill({ status: 502, contentType: 'text/plain', body: 'inventory upstream unavailable' }),
    );
    await mountPage(page, '/ux/shop');

    await expect(page.locator(FAILURE_MARKER)).toHaveText([
      "Couldn't load stock levels — /api/inventory/items: HTTP 502. Availability below is unknown, not sold out.",
    ]);
    await expect(page.locator(FAILURE_MARKER)).toHaveAttribute('role', 'alert');
    // The catalog still renders: the failure is one line, not the page.
    await expect(card(page, 'Algedonic Pale Ale')).toBeVisible();
  });

  test('an unreachable stock read is named too, not swallowed', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(ITEMS, (r: Route) => r.abort('connectionrefused'));
    await mountPage(page, '/ux/shop');

    await expect(page.locator(FAILURE_MARKER)).toHaveText([
      /Couldn't load stock levels — \/api\/inventory\/items: .+\. Availability below is unknown, not sold out\./,
    ]);
  });

  test('an answered stock read paints its figures and no failure line', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(ITEMS, (r: Route) =>
      r.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify([{ part_sku: 'FP-PALE-1-2-BBL', on_hand: 20, allocated: 2 }]),
      }),
    );
    await mountPage(page, '/ux/shop');

    await expect(card(page, 'Algedonic Pale Ale').locator('.shop-card-avail')).toHaveText('18 in stock');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });
});
