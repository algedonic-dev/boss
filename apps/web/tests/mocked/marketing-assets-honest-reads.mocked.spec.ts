// /ux/marketing-assets — every read the two pages make says when it
// failed, and the list says what it could not show.
//
// Page audit 7cdb095b, and the items its `test` step pins (eac5d4c8):
//   - the list read's failure line, instead of "No marketing assets yet."
//   - the Class registry read on BOTH pages (e520c794): it used to fail
//     silently — the Kind rail fell to "All" alone and kinds printed as
//     raw codes, with no line saying why
//   - the route rendering the module-off notice when the manifest omits
//     `marketing-assets`, in words true of every instance (fa838818)
// and the list fixes riding the same car:
//   - the Owner column names people, one read per distinct owner, and
//     says so when a name cannot load (7ea34901)
//   - an answer holding exactly the limit says there may be more (a14de49e)
//   - no subtitle enumerating kinds the registry decides (e2b6a801)
//
// Paths come from the catalog, so the route move (4b8ddb60) carries
// this spec with it.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import {
  installSmokeMocks, installTenantManifest, LIVE_MANIFEST_RECORDED_AT, MARKETING_ASSET_DETAIL,
  MODULES_LIVE,
} from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG['marketing-assets'].path;
const LIST = /\/api\/catalog\/marketing-assets(\?|$)/;
const HISTORY = /\/api\/catalog\/marketing-assets\/[^/]+\/history$/;
// Only this page's Class read: the shell reads its own classes through
// the same endpoint and must keep painting.
const KIND_CLASSES = /\/api\/classes\?subject_kind=marketing-asset$/;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const asset = (id: string, over: Record<string, unknown> = {}) => ({
  id, title: `Asset ${id}`, kind: 'photo', description: null, file_url: null,
  tags: [], linked_skus: [], linked_account_ids: [], linked_campaign_ids: [],
  owner_id: null, brand_reviewed_by: null, brand_reviewed_at: null,
  supersedes_id: null, retired_at: null,
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-02T00:00:00Z',
  ...over,
});

const KIND_ROWS = [
  { subject_kind: 'marketing-asset', code: 'photo', display_name: 'Photograph', parent_code: null, member_attribute: 'kind', metadata: {}, sort_order: 1, retired_at: null },
  { subject_kind: 'marketing-asset', code: 'deck', display_name: 'Slide deck', parent_code: null, member_attribute: 'kind', metadata: {}, sort_order: 2, retired_at: null },
];

type Legs = Readonly<{
  list?: (r: Route) => Promise<void>;
  kinds?: (r: Route) => Promise<void>;
  person?: (r: Route) => Promise<void>;
}>;

/// Every read answered, then the legs a test is about replace theirs.
/// Registered after the smoke mocks, so these win.
async function install(page: Page, legs: Legs = {}): Promise<void> {
  await installSmokeMocks(page);
  await page.route(LIST, legs.list ?? ((r) => json(r, [asset('ma-1', { owner_id: 'emp-mkt-1' })])));
  await page.route(KIND_CLASSES, legs.kinds ?? ((r) => json(r, KIND_ROWS)));
  await page.route(/\/api\/people\/emp-mkt-1$/, legs.person ?? ((r) => json(r, { id: 'emp-mkt-1', name: 'Mara Lind' })));
  await page.route(MARKETING_ASSET_DETAIL, (r) => json(r, asset('ma-1', { owner_id: 'emp-mkt-1' })));
  await page.route(HISTORY, (r) => json(r, [asset('ma-1')]));
}

const kindRail = (page: Page) => page.locator('.filter-group', { has: page.locator('.filter-label', { hasText: /^Kind$/ }) });
const title = (page: Page) => page.locator('h1.exec-title');
const rows = (page: Page) => page.locator('.list-section tbody tr');

test.describe(`${PATH} — the list's reads`, () => {
  test('answered, the rail lists the registry kinds by label, owners are named, and nothing says a read failed', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);

    await expect(rows(page)).toHaveCount(1);
    await expect(kindRail(page).getByRole('button')).toHaveText(['All', 'Photograph', 'Slide deck']);
    await expect(rows(page).locator('td').nth(1)).toHaveText('Photograph');
    await expect(rows(page).locator('td').nth(4).getByRole('link')).toHaveText('Mara Lind');
    await expect(title(page)).toHaveText('Marketing assets (1)');
    await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
    // e2b6a801: no sentence enumerating the kinds — the rail is the list.
    await expect(page.locator('.exec-header p')).toHaveCount(0);
  });

  test('a refused list read names the failure, not "No marketing assets yet."', async ({ page }) => {
    await install(page, { list: (r) => json(r, { error: 'catalog down' }, 500) });
    await mountPage(page, PATH);

    await expect(page.locator(`.list-section ${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load marketing assets — /api/catalog/marketing-assets: HTTP 500",
    );
    await expect(page.locator('.list-section')).not.toContainText('No marketing assets yet.');
  });

  test('a refused Class read says so beside the rail, and kinds show as their codes', async ({ page }) => {
    await install(page, { kinds: (r) => json(r, { error: 'classes down' }, 503) });
    await mountPage(page, PATH);

    await expect(kindRail(page).locator(`${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the asset kinds — /api/classes?subject_kind=marketing-asset: HTTP 503. Kinds show as codes.",
    );
    await expect(kindRail(page).getByRole('button')).toHaveText(['All']);
    await expect(rows(page).locator('td').nth(1)).toHaveText('photo');
  });

  test("a refused owner read keeps the id and says the names failed", async ({ page }) => {
    await install(page, { person: (r) => json(r, { error: 'people down' }, 503) });
    await mountPage(page, PATH);

    await expect(rows(page).locator('td').nth(4).getByRole('link')).toHaveText('emp-mkt-1');
    await expect(page.locator(`.list-section ${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the owners' names — /api/people/emp-mkt-1: HTTP 503. People show as ids.",
    );
  });

  test('an answer holding exactly the limit says there may be more', async ({ page }) => {
    const full = Array.from({ length: 500 }, (_, i) => asset(`ma-${i}`));
    await install(page, { list: (r) => json(r, full) });
    await mountPage(page, PATH);

    await expect(title(page)).toHaveText('Marketing assets (500+)');
    await expect(page.locator('.list-section .list-truncated')).toHaveText(
      'This list shows the first 500 assets, and there may be more — narrow it by kind.',
    );
  });

  test('an answer under the limit says nothing about more', async ({ page }) => {
    const some = Array.from({ length: 499 }, (_, i) => asset(`ma-${i}`));
    await install(page, { list: (r) => json(r, some) });
    await mountPage(page, PATH);

    await expect(title(page)).toHaveText('Marketing assets (499)');
    await expect(page.locator('.list-truncated')).toHaveCount(0);
  });
});

test.describe(`${PATH}/{id} — the Class read on the detail page`, () => {
  const eyebrow = (page: Page) => page.locator('.detail-hero .detail-eyebrow');

  test('answered, the eyebrow names the kind by its label', async ({ page }) => {
    await install(page);
    await mountPage(page, `${PATH}/ma-1`);

    await expect(eyebrow(page)).toContainText('Photograph');
    await expect(page.locator('.detail-hero').locator(FAILURE_MARKER)).toHaveCount(0);
  });

  test('refused, the kind shows as its code and a line beside the eyebrow says why', async ({ page }) => {
    await install(page, { kinds: (r) => json(r, { error: 'classes down' }, 503) });
    await mountPage(page, `${PATH}/ma-1`);

    await expect(eyebrow(page)).toContainText('photo');
    await expect(page.locator(`.detail-hero ${FAILURE_MARKER}[role=alert]`)).toHaveText(
      "Couldn't load the asset kinds — /api/classes?subject_kind=marketing-asset: HTTP 503. Kinds show as codes.",
    );
  });
});

test.describe(`${PATH}/{id} — linked SKUs`, () => {
  // f925b58b (page audit 7cdb095b gap 9): the field was
  // `linked_device_skus` and each id was routed by prefix — `FP-` to
  // /ux/products, anything else to /ux/catalog, the retired device
  // shop's page. The field is `linked_skus` now and every SKU is a
  // product: no id-prefix convention in shared web code.
  test('every linked SKU links to its product page, whatever its prefix', async ({ page }) => {
    await install(page);
    await page.route(MARKETING_ASSET_DETAIL, (r) =>
      json(r, asset('ma-1', { linked_skus: ['FP-KOLSCH-12OZ', 'SIXTEL-7'] })));
    await mountPage(page, `${PATH}/ma-1`);

    const skus = page.locator('.linked-skus a');
    await expect(skus).toHaveText(['FP-KOLSCH-12OZ', 'SIXTEL-7']);
    await expect(skus.nth(0)).toHaveAttribute('href', /\/ux\/products\/FP-KOLSCH-12OZ$/);
    await expect(skus.nth(1)).toHaveAttribute('href', /\/ux\/products\/SIXTEL-7$/);
    await expect(page.getByText('Device SKUs')).toHaveCount(0);
  });
});

test.describe(`${PATH} — the module off`, () => {
  test(`the live manifest recorded ${LIVE_MANIFEST_RECORDED_AT} (no marketing-assets key) renders the notice, naming the manifest and not a path`, async ({ page }) => {
    await install(page);
    await installTenantManifest(page, MODULES_LIVE, { inline: true });
    await mountPage(page, PATH);

    const notice = page.locator('.module-disabled');
    await expect(notice.locator('h1')).toHaveText('Not enabled for this tenant');
    await expect(notice.locator('strong')).toHaveText(ROUTE_CATALOG['marketing-assets'].label);
    await expect(notice).toContainText(
      "The Marketing assets module is turned off in this tenant's manifest. The page exists in the platform — the active tenant just doesn't surface it.",
    );
    // fa838818 / cbe6bc94: the manifest by what it is — no examples/ path,
    // which is true of one demo tenant and false of every other instance.
    await expect(notice).toContainText(
      "To enable: set marketing-assets = true in the [modules] section of the tenant's manifest, redeploy, and the page comes back.",
    );
    await expect(notice).not.toContainText('examples/');
    await expect(page.locator('.list-section')).toHaveCount(0);
  });
});
