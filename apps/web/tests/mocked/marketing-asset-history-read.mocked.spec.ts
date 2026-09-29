// /ux/marketing-assets/{id} — a refused history read is not "no prior
// versions".
//
// Backlog 865d3d51. The page read the asset's version history with an
// `if (hResp.ok)` and no else, so a refusal left the history empty and
// the page said "No prior versions — this is the original." and
// "Versions 1" — claims only an answered read may make. The read now
// keeps its ReadState (src/data/readState.ts) and the section names the
// failure; the Versions figure reads "?".

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, MARKETING_ASSET_DETAIL } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';

const PATH = '/ux/marketing-assets/ma-hist';
const HISTORY = /\/api\/catalog\/marketing-assets\/[^/]+\/history$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const ASSET = {
  id: 'ma-hist', title: 'Autumn tap list', kind: null, description: null, file_url: null,
  tags: [], linked_skus: [], linked_account_ids: [], linked_campaign_ids: [],
  owner_id: null, brand_reviewed_by: null, brand_reviewed_at: null,
  supersedes_id: null, retired_at: null,
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-02T00:00:00Z',
};

async function install(page: Page, history: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(MARKETING_ASSET_DETAIL, (r) => json(r, ASSET));
  await page.route(HISTORY, history);
}

const historySection = (page: Page) =>
  page.locator('section', { has: page.getByRole('heading', { name: /^Version history/ }) });

test.describe('/ux/marketing-assets/{id} — the version history read', () => {
  test('answered with only the asset, the page says it is the original, and nothing says a read failed', async ({ page }) => {
    await install(page, (r) => json(r, [ASSET]));
    await mountPage(page, PATH);

    await expect(historySection(page)).toContainText('No prior versions — this is the original.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a 500 names the failure, not "No prior versions"', async ({ page }) => {
    await install(page, (r) => json(r, { error: 'catalog down' }, 500));
    await mountPage(page, PATH);

    await expect(historySection(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the version history — /api/catalog/marketing-assets/ma-hist/history: HTTP 500",
    );
    await expect(historySection(page)).not.toContainText('No prior versions');
    await expect(page.getByRole('heading', { name: /^Version history/ })).toHaveText('Version history (?)');
  });
});
