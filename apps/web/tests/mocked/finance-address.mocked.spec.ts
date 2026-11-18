// Finance's pages live at Finance's address (backlog c7ef4728, car 4 of
// design 8c3e9599; design deba3000: a page belongs to the station that
// hosts it, and the URL is that station's address).
//
// Two halves, measured the way a reader meets them. The new addresses
// render their pages inside the Finance tab. The old /ux spellings are
// NOT redirected: David retired /ux outright on 2026-09-25 ("no legacy
// field, no aliases or redirects; old /ux links simply stop
// resolving"), so each renders the not-found page AT the old address,
// which stays in the bar as the evidence (design ee3a3a2f).

import { expect, test } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, signInWithPolicy } from './_smokeMocks';

const NOT_FOUND = 'No page at this address';

test.describe('Finance renders at /finance', () => {
  test('/finance is the Finance page, under the Finance tab and its own sidebar row', async ({ page }) => {
    await installSmokeMocks(page);
    await signInWithPolicy(page);
    await mountPage(page, '/finance');
    expect(new URL(page.url()).pathname).toBe('/finance');
    await expect(page.getByRole('heading', { name: NOT_FOUND })).toHaveCount(0);
    await expect(page.locator('.perspective-tabs [aria-current="page"]')).toContainText('Finance');
    await expect(page.getByRole('link', { name: '+ New invoice' })).toHaveAttribute('href', '/finance/invoices/new');
  });

  for (const path of ['/finance/invoices/new', '/finance/journal-entries/new', '/finance/invoices/inv-review']) {
    test(`${path} renders a Finance page, not the not-found page`, async ({ page }) => {
      await installSmokeMocks(page);
      await signInWithPolicy(page);
      // The invoice read answers 404, so the invoice page renders its own
      // not-found state: the floor's `[]` is no invoice, and the page
      // reads one without parsing it.
      await page.route(/\/api\/commerce\/invoices\/inv-review$/, (r) => r.fulfill({ status: 404, body: '' }));
      await mountPage(page, path);
      expect(new URL(page.url()).pathname).toBe(path);
      await expect(page.locator('h1').first()).toBeVisible();
      await expect(page.getByRole('heading', { name: NOT_FOUND })).toHaveCount(0);
      await expect(page.locator('.perspective-tabs [aria-current="page"]')).toContainText('Finance');
    });
  }
});

test.describe('the retired /ux spellings answer nothing, and are not redirected', () => {
  for (const path of [
    '/ux/finance',
    '/ux/finance/new',
    '/ux/finance/journal-entries/new',
    '/ux/vendors',
    '/ux/purchase-orders/po-review',
    '/ux/vendor-invoices/bill-review',
  ]) {
    test(`${path} is not found, and stays at ${path}`, async ({ page }) => {
      await installSmokeMocks(page);
      await signInWithPolicy(page);
      await mountPage(page, path, { titleMatch: new RegExp(NOT_FOUND) });
      // Rendered in place, never redirected: the dead path is the evidence.
      expect(new URL(page.url()).pathname).toBe(path);
      await expect(page.locator('.app-shell')).toContainText(`Nothing in this app answers ${path}`);
    });
  }
});
