// An unmatched path says so, and names the path (design ee3a3a2f,
// backlog c4f2ae24).
//
// Until then the router's catch-all rendered the landing page for /ux
// and the yard for /it, and the greedy single-id wildcards turned two
// dead links into something worse: /ux/accounts/agreements/<id> rendered
// the ACCOUNT page reporting a missing account "agreements/<id>", and
// /ux/sales/opportunities/<id> the JOB page. Each was a real, working,
// plausible page answering the wrong question, and nobody files a packet
// about a page that looks fine. This renders the not-found surface the
// way a reader meets it: at the dead URL, with the URL left in place.

import { expect, test } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';

const TITLE = 'No page at this address';

test.describe('an unmatched path renders a not-found that names it', () => {
  for (const path of ['/ux/accounts/agreements/x', '/ux/sales/opportunities/x', '/ux/no-such-route']) {
    test(`${path} says there is nothing here, and stays at ${path}`, async ({ page }) => {
      await installSmokeMocks(page);
      await mountPage(page, path, { titleMatch: new RegExp(TITLE) });

      await expect(page.locator('.app-shell')).toContainText(`Nothing in this app answers ${path}`);
      // Rendered in place, never redirected: the dead path is the evidence.
      expect(new URL(page.url()).pathname).toBe(path);
      const back = page.getByRole('link', { name: /Back to My Day/ });
      await expect(back).toHaveAttribute('href', '/me');
      // The bar walks the station tree since backlog 474e2c1c: a path no
      // station answers stands at the company root, where its door leads.
      await expect(page.locator('.perspective-tabs [aria-current="page"]')).toHaveAttribute('data-trail-stop', 'company');
    });
  }

  // It said so inside IT, with its door back to the alarms board, until
  // backlog 92bfcb04 retired the /it prefix with IT as a department
  // (design df0ecd8b): a path under it is a path like any other.
  test('an unknown /it path says so at the company root, with its one door back to My Day', async ({ page }) => {
    await installSmokeMocks(page);
    await mountPage(page, '/it/no-such', { titleMatch: new RegExp(TITLE) });

    await expect(page.locator('.app-shell')).toContainText('Nothing in this app answers /it/no-such');
    expect(new URL(page.url()).pathname).toBe('/it/no-such');
    await expect(page.getByRole('link', { name: /Back to My Day/ })).toHaveAttribute('href', '/me');
    // At the company root, where its door leads (backlog 474e2c1c: the bar
    // walks the station tree).
    await expect(page.locator('.perspective-tabs [aria-current="page"]')).toHaveAttribute('data-trail-stop', 'company');
  });
});

test.describe('the landing page keeps a door of its own', () => {
  test('/ux/system-model renders the landing page the catch-all used to serve', async ({ page }) => {
    await installSmokeMocks(page);
    await mountPage(page, '/ux/system-model');
    await expect(page.locator('h1').first()).toHaveText('BOSS');
    await expect(page.locator('.app-shell')).not.toContainText(TITLE);
  });
});
