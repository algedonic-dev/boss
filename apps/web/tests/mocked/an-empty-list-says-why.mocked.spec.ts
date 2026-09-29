// An empty list says WHICH empty it is — backlog 0ef5e008.
//
// Seven list pages had one empty branch, `visible.length === 0`, which
// printed "No X match those filters." whether nothing existed, the
// filters hid everything, or (one layer down) a 200 of the wrong shape
// had been coerced to no rows. The shared helper (src/data/readState.ts
// `emptyState` / `emptyLine`, rendered by src/data/ListEmpty.svelte)
// answers three different lines, and a failed read names the read.
//
// Most of the seven have a spec of their own, and their empty, filtered,
// failed and wrong-shape cases live there (accounts-, people-, parts-,
// finance-, inbox-, vendors-page). /ux/shipping and /ux/catalog had no
// spec of their own at all, so their four cases are pinned here.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const section = (page: Page) => page.locator('section.list-section, section.catalog-grid');
const line = (page: Page) => section(page).locator('p.empty');

// ── /ux/shipping ─────────────────────────────────────────────────────

const SHIPPING = ROUTE_CATALOG.shipping.path;
const SHIPMENTS = /\/api\/shipping\/shipments(\?|$)/;
const SHIPMENTS_URL = '/api/shipping/shipments?limit=1000';

const shipment = (id: string, status: string) => ({
  id, direction: 'outbound', status, carrier: null, tracking_number: null,
  origin: 'HQ', destination: 'Depot', asset_ids: [], line_items: [],
  po_id: null, order_id: null, account_id: null,
  created_on: '2026-09-01', shipped_on: null, estimated_delivery: null, delivered_on: null,
});

const page1 = (rows: ReadonlyArray<unknown>) => ({ data: rows, total: rows.length, limit: 1000, offset: 0 });

test.describe('/ux/shipping — an empty list says which empty it is', () => {
  test('no shipments at all says so, not that the filters hid them', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(SHIPMENTS, (r) => json(r, page1([])));
    await mountPage(page, SHIPPING);
    await expect(line(page)).toHaveText('No shipments yet.');
    await expect(page.getByText('No shipments match those filters.')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a book of delivered shipments under the default Undelivered filter is the filters line', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(SHIPMENTS, (r) => json(r, page1([shipment('sh-9', 'delivered')])));
    await mountPage(page, SHIPPING);
    await expect(line(page)).toHaveText('No shipments match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a refused read is the failure line, naming the read', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(SHIPMENTS, (r) => json(r, { error: 'shipping down' }, 503));
    await mountPage(page, SHIPPING);
    await expect(section(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      `Couldn't load shipments — ${SHIPMENTS_URL}: HTTP 503`,
    );
    await expect(page.getByText('No shipments yet.')).toHaveCount(0);
  });

  test('a 200 that is not the envelope is a failed read, never an empty book', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(SHIPMENTS, (r) => json(r, [shipment('sh-1', 'in-transit')]));
    await mountPage(page, SHIPPING);
    await expect(section(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      `Couldn't load shipments — ${SHIPMENTS_URL}: HTTP 200, but the body is a list, not a {data: [...]} envelope`,
    );
    await expect(page.getByText('No shipments yet.')).toHaveCount(0);
  });
});

// ── /ux/catalog ──────────────────────────────────────────────────────

const CATALOG = ROUTE_CATALOG.catalog.path;
const MODELS = /\/api\/catalog\/models$/;

const model = (sku: string, name: string) => ({
  sku, name, manufacturer: 'Acme', model_year: 2024, category: 'brewhouse',
  extras: null, physical: null, regulatory: null,
  commerce: {
    list_price_new_cents: 100_000, typical_refurb_price_cents: null, currency: 'USD',
    lead_time_days: null, tagline: 'Mash and lauter', description: '', use_cases: ['mashing'], hero_image: null,
  },
  service: {
    preventive_maintenance_hours: 2, preventive_maintenance_interval_months: 6,
    calibration_interval_months: 12, required_skill_level: 2, depot_required: false,
    common_failure_modes: [], pm_checklist: [],
  },
  spare_parts: [], consumables: [], documents: [], end_of_support: null, current_firmware: null,
});

test.describe('/ux/catalog — an empty list says which empty it is', () => {
  test('an empty catalog says so, not that the filters hid it', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(MODELS, (r) => json(r, []));
    await mountPage(page, CATALOG);
    await expect(line(page)).toHaveText('No catalog systems yet.');
    await expect(page.getByText('No catalog systems match those filters.')).toHaveCount(0);
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('models the search hides are the filters line', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(MODELS, (r) => json(r, [model('MT-10', 'Mash tun')]));
    await mountPage(page, CATALOG);
    await expect(page.locator('.catalog-card-name')).toHaveText(['Mash tun']);
    await page.getByRole('searchbox', { name: 'Search the catalog' }).fill('no such device');
    await expect(line(page)).toHaveText('No catalog systems match those filters.');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('a refused read is the failure line, naming the read', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(MODELS, (r) => json(r, { error: 'catalog down' }, 503));
    await mountPage(page, CATALOG);
    await expect(section(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the catalog — /api/catalog/models: HTTP 503",
    );
  });

  test('a 200 that is neither list shape is a failed read, never an empty catalog', async ({ page }) => {
    await installSmokeMocks(page);
    await page.route(MODELS, (r) => json(r, { error: 'contract changed' }));
    await mountPage(page, CATALOG);
    await expect(section(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the catalog — /api/catalog/models: HTTP 200, but the body is an object with no data list, not a list or a {data: [...]} envelope",
    );
    await expect(page.getByText('No catalog systems yet.')).toHaveCount(0);
  });
});
