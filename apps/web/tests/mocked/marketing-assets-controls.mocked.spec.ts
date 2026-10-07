import { isPageWrite, pageWrites } from './_smokeMocks';
// Page audit 7cdb095b, whole-control completion after eac5d4c8's
// honest-read car. The three existing marketing specs retain the
// classes/people/history/truncation refusal pins; this one adds the
// rail, search, all six sort controls, links and Back, and empty shapes.
// No curation form exists: assert zero page writes instead of inventing
// a refused write. The tenant's module stays off outside these mocks.

import { expect, test, type Page, type Route } from './_test';
import { mountPage, openedRequests, recordPageRequests } from './_helpers';
import { installSmokeMocks, installTenantManifest, MODULES_LIVE } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';
import { parseRoute } from '../../src/router';
import type { MarketingAsset } from '../../src/marketing-assets/types';
import { HOME_APP } from '@boss/web-kit/nav';
import type { ProductDetail } from '../../src/products/types';
import type { Employee } from '../../src/people/types';
import { AccountSchema } from '../../src/accounts/schemas';
import type { Job } from '../../src/jobs/types';

const PATH = ROUTE_CATALOG['marketing-assets'].path;
const LIST = /\/api\/catalog\/marketing-assets(\?|$)/;
const DETAIL = /\/api\/catalog\/marketing-assets\/[^/?]+$/;
const HISTORY = /\/api\/catalog\/marketing-assets\/[^/?]+\/history$/;
const json = (r: Route, body: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const asset = (id: string, extra: Partial<MarketingAsset> = {}): MarketingAsset => ({
  id, title: id, kind: 'photo', description: null, file_url: null,
  tags: [], linked_skus: [], linked_account_ids: [], linked_campaign_ids: [],
  owner_id: null, brand_reviewed_by: null, brand_reviewed_at: null,
  supersedes_id: null, retired_at: null,
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-02T00:00:00Z', ...extra,
});
const A = asset('ma-alpha', {
  title: 'Alpha artwork', description: 'Seasonal illustration', owner_id: 'emp-z',
  tags: ['amber', 'taproom', 'autumn', 'print', 'extra'],
  linked_skus: ['SKU-A'], linked_account_ids: ['acc-a'], linked_campaign_ids: ['campaign-a'],
  updated_at: '2026-09-03T00:00:00Z',
});
const B = asset('ma-beta', {
  title: 'Beta deck', kind: 'deck', tags: ['slides'], owner_id: 'emp-a',
  linked_skus: ['SKU-B'], updated_at: '2026-09-04T00:00:00Z',
});
const C = asset('ma-gamma', { title: 'Gamma retired', retired_at: '2026-09-05T00:00:00Z' });
const ROWS = [A, B, C];
const KINDS = ['photo', 'deck'].map((code, i) => ({
  subject_kind: 'marketing-asset', code, display_name: i ? 'Slide deck' : 'Photograph',
  parent_code: null, member_attribute: 'kind', metadata: {}, sort_order: i, retired_at: null,
}));
type Seen = { reads: URLSearchParams[]; writes: string[] };
async function install(page: Page): Promise<Seen> {
  const seen: Seen = { reads: [], writes: [] };
  page.on('request', (r) => {
    const path = new URL(r.url()).pathname;
    if (isPageWrite(r.method(), path)) {
      seen.writes.push(`${r.method()} ${path}`);
    }
  });
  await installSmokeMocks(page);
  await page.route(/\/api\/classes\?subject_kind=marketing-asset$/, (r) => json(r, KINDS));
  await page.route(/\/api\/people\/emp-[az]$/, (r) => {
    const id = new URL(r.request().url()).pathname.split('/').pop();
    return json(r, { id, name: id === 'emp-a' ? 'Ada Owner' : 'Zed Owner' });
  });
  await page.route(LIST, (r) => {
    const q = new URL(r.request().url()).searchParams;
    seen.reads.push(q);
    return json(r, ROWS.filter((a) => (!q.get('kind') || a.kind === q.get('kind')) &&
      (q.get('include_retired') === 'true' || !a.retired_at)));
  });
  await page.route(DETAIL, (r) => {
    const id = decodeURIComponent(new URL(r.request().url()).pathname.split('/').pop() ?? '');
    return json(r, ROWS.find((a) => a.id === id) ?? asset(id));
  });
  await page.route(HISTORY, (r) => json(r, []));
  return seen;
}
const root = (page: Page) => page.locator('.catalog.theme-exec');
const rows = (page: Page) => root(page).locator('tbody tr');
const titles = (page: Page) => rows(page).locator('td:first-child a');
const button = (page: Page, name: string) => root(page).getByRole('button', { name, exact: true });
const line = (page: Page) => root(page).locator('.list-section p.empty');
const search = (page: Page) => page.getByRole('searchbox', { name: 'Search marketing assets' });
const last = (s: Seen) => Object.fromEntries(s.reads.at(-1) ?? []);

test('pending list and detail reads say Loading until their actual answers arrive', async ({ page }) => {
  await install(page);
  let listRead: Route | undefined;
  await page.route(LIST, (r) => { listRead = r; });
  await mountPage(page, PATH);
  await expect(line(page)).toHaveText('Loading…');
  await expect(root(page).locator('h1')).toHaveText('Marketing assets (0…)');
  await expect.poll(() => !!listRead).toBe(true);
  if (!listRead) throw new Error('List read did not reach the fixture');
  await json(listRead, [A]);
  await expect(titles(page)).toHaveText([A.title]);
  let detailRead: Route | undefined;
  await page.route(DETAIL, (r) => { detailRead = r; });
  await titles(page).click();
  await expect(page.locator('.detail-page p.empty')).toHaveText('Loading…');
  await expect.poll(() => !!detailRead).toBe(true);
  if (!detailRead) throw new Error('Detail read did not reach the fixture');
  await json(detailRead, A);
  await expect(page.locator('h1.detail-title')).toHaveText(A.title);
});

test('inventory, labels, row cells and all kind/status buttons describe the actual backend', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  await expect(titles(page)).toHaveText(['Beta deck', 'Alpha artwork']);
  await expect(root(page).locator('.exec-eyebrow')).toHaveText('Know');
  await expect(root(page).locator('h1')).toHaveText('Marketing assets (2)');
  await expect(root(page).locator('.exec-header p')).toHaveCount(0);
  await expect(root(page).getByRole('button')).toHaveText(['All', 'Photograph', 'Slide deck', 'Active only', 'Include retired']);
  await expect(root(page).locator('thead th')).toHaveText(['Asset', 'Kind', 'Tags', 'Linked', 'Owner', 'Updated ↓']);
  await expect(search(page)).toHaveAttribute('placeholder', 'Title, tag, SKU, campaign…');
  await expect(root(page).locator('form')).toHaveCount(0);
  await expect(rows(page).nth(1).locator('td').nth(2)).toContainText('amber, taproom, autumn, print +1');
  await expect(rows(page).nth(1).locator('td').nth(3)).toHaveText('3 links');
  await expect(rows(page).nth(0).locator('td').nth(3)).toHaveText('1 link');
  await expect(rows(page).nth(1).locator('td').nth(4)).toHaveText('Zed Owner');
  expect(last(seen)).toEqual({ limit: '500' });
  for (const [label, kind] of [['Photograph', 'photo'], ['Slide deck', 'deck']] as const) {
    await button(page, label).click();
    await expect.poll(() => last(seen)).toEqual({ kind, limit: '500' });
    await expect(titles(page)).toHaveText([kind === 'photo' ? A.title : B.title]);
    await expect(button(page, label)).toHaveAttribute('aria-pressed', 'true');
  }
  await button(page, 'All').click();
  await expect(titles(page)).toHaveCount(2);
  await button(page, 'Include retired').click();
  await expect.poll(() => last(seen)).toEqual({ include_retired: 'true', limit: '500' });
  await expect(titles(page)).toHaveText([B.title, A.title, C.title]);
  await expect(rows(page).nth(2)).toHaveCSS('opacity', '0.55');
  await expect(rows(page).nth(2)).toContainText('RETIRED');
  await button(page, 'Active only').click();
  await expect(titles(page)).toHaveCount(2);
  await expect(button(page, 'Active only')).toHaveAttribute('aria-pressed', 'true');
  expect(seen.writes).toEqual([]);
});

for (const [label, initial] of [
  ['Asset', ['Alpha artwork', 'Beta deck']], ['Kind', ['Beta deck', 'Alpha artwork']],
  ['Tags', ['Alpha artwork', 'Beta deck']], ['Linked', ['Alpha artwork', 'Beta deck']],
  ['Owner', ['Beta deck', 'Alpha artwork']], ['Updated', ['Alpha artwork', 'Beta deck']],
] as const) {
  test(`${label} sort changes order, then keyboard toggles it back without another read`, async ({ page }) => {
    const seen = await install(page);
    await mountPage(page, PATH);
    await expect(titles(page)).toHaveCount(2);
    await expect(rows(page).nth(0).locator('td').nth(4)).toHaveText('Ada Owner');
    const count = seen.reads.length;
    const head = root(page).getByRole('columnheader', { name: new RegExp(`^${label}( [↑↓])?$`) });
    await head.click();
    await expect(titles(page)).toHaveText([...initial]);
    await expect(head).toHaveAttribute('aria-sort', ['Tags', 'Linked'].includes(label) ? 'descending' : 'ascending');
    await head.press('Enter');
    await expect(titles(page)).toHaveText([...initial].reverse());
    await head.press(' ');
    await expect(titles(page)).toHaveText([...initial]);
    expect(seen.reads).toHaveLength(count);
    expect(seen.writes).toEqual([]);
  });
}

test('search covers identity, title, description, tags, SKU and campaign, clears, and never claims an absent source', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  await expect(titles(page)).toHaveCount(2);
  const before = seen.reads.length;
  for (const term of ['ma-alpha', 'ALPHA', 'illustration', 'amber', 'SKU-A', 'campaign-a']) {
    await search(page).fill(term);
    await expect(titles(page)).toHaveText([A.title]);
    await expect(root(page).locator('h1')).toHaveText('Marketing assets (2)');
  }
  // Linked account ids contribute to Linked, but are not in the documented search haystack.
  await search(page).fill('acc-a');
  await expect(line(page)).toHaveText('No assets match those filters.');
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  await search(page).fill('');
  await expect(titles(page)).toHaveCount(2);
  expect(seen.reads).toHaveLength(before);
  expect(seen.writes).toEqual([]);
});

test('each kind/status refetch reports refusal where rows were, then a real empty response uses the selected kind', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  await expect(titles(page)).toHaveCount(2);
  await page.route(LIST, (r) => json(r, { error: 'catalog refused' }, 503));
  for (const label of ['Photograph', 'Slide deck', 'All', 'Include retired', 'Active only']) {
    await button(page, label).click();
    await expect(line(page)).toHaveText("Couldn't load marketing assets — /api/catalog/marketing-assets: HTTP 503");
    await expect(line(page)).toHaveClass(/load-failed/);
    await expect(line(page)).toHaveAttribute('role', 'alert');
    await expect(titles(page)).toHaveCount(0);
  }
  await page.unroute(LIST);
  await page.route(LIST, (r) => json(r, []));
  await button(page, 'Photograph').click();
  await expect(line(page)).toHaveText('No Photograph assets yet.');
  await button(page, 'All').click();
  await expect(line(page)).toHaveText('No marketing assets yet.');
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  expect(seen.writes).toEqual([]);
});

for (const [label, body] of [['object', { data: [] }], ['null', null], ['string', 'no rows']] as const) {
  test(`malformed ${label} 200 cannot wear the empty-source state`, async ({ page }) => {
    await install(page);
    await page.route(LIST, (r) => json(r, body));
    await mountPage(page, PATH);
    await expect(line(page)).toContainText('HTTP 200, but the body is');
    await expect(line(page)).toHaveClass(/load-failed/);
    await expect(line(page)).not.toContainText('No marketing assets yet.');
  });
}

test('title, row click and keyboard reach the asset, and each Back restores the list; Owner reaches people', async ({ page }) => {
  const seen = await install(page);
  await mountPage(page, PATH);
  const target = `${PATH}/${A.id}`;
  expect(parseRoute(target)).toMatchObject({ kind: 'marketingAsset' });
  for (const gesture of ['title', 'row', 'Enter', ' '] as const) {
    const row = rows(page).filter({ has: page.getByRole('link', { name: A.title, exact: true }) });
    await expect(row).toHaveCount(1);
    if (gesture === 'title') await row.getByRole('link', { name: A.title, exact: true }).click();
    else if (gesture === 'row') await row.locator('td').nth(2).click();
    else await row.press(gesture);
    await expect.poll(() => new URL(page.url()).pathname).toBe(target);
    await expect(page.locator('h1.detail-title')).toHaveText(A.title);
    await page.goBack();
    await expect(titles(page)).toHaveCount(2);
  }
  const owner = rows(page).getByRole('link', { name: 'Zed Owner', exact: true });
  await expect(owner).toHaveAttribute('href', `${ROUTE_CATALOG.people.path}/emp-z`);
  await owner.click();
  await expect.poll(() => new URL(page.url()).pathname).toBe(`${ROUTE_CATALOG.people.path}/emp-z`);
  await page.goBack();
  await expect(titles(page)).toHaveCount(2);
  expect(seen.writes).toEqual([]);
});

test('module-off has one real home button, Back restores the notice, and the page performs no reads or writes', async ({ page }) => {
  const seen = await install(page);
  await installTenantManifest(page, MODULES_LIVE, { inline: true });
  await mountPage(page, PATH);
  const notice = page.locator('.module-disabled');
  await expect(notice.getByRole('heading')).toHaveText('Not enabled for this tenant');
  await expect(notice.getByRole('button')).toHaveText('Back to home');
  await expect(notice.locator('a, form')).toHaveCount(0);
  expect(seen.reads).toEqual([]);
  // ModuleDisabled navigates('/'); shared nav owns Home and the router
  // renders MePage there. A different route is not evidence of Home.
  expect(parseRoute(HOME_APP.href)).toEqual({ kind: 'me' });
  await notice.getByRole('button').click();
  await expect.poll(() => new URL(page.url()).pathname).toBe(HOME_APP.href);
  const home = page.locator('.theme-exec p.empty', { hasText: 'Not signed in.' });
  await expect(home).toHaveText('Not signed in. Sign in to see your day.');
  await expect(home.getByRole('link', { name: 'Sign in', exact: true })).toHaveAttribute('href', '/login');
  await page.goBack();
  await expect(notice).toBeVisible();
  expect(seen.reads).toEqual([]);
  expect(seen.writes).toEqual([]);
});

const detail = (page: Page) => page.locator('.detail-page.theme-exec');
const profile = (page: Page) => detail(page).locator('section', { has: page.getByRole('heading', { name: 'Profile', exact: true }) });
const cell = (page: Page, name: string) => profile(page).locator(`dt:text-is("${name}") + dd`);

test('the detail inventory names every link, follows its catalogued destination, and returns with Back', async ({ page }) => {
  const seen = await install(page);
  await recordPageRequests(page);
  // The generic smoke floor answers unseeded reads with [], which is
  // not ProductDetail. Gate c4ee caught inventory.length racing Back.
  // Each activated detail destination gets its own valid private row.
  const product: ProductDetail = {
    sku: 'SKU-A', name: 'Linked product A', product_kind: 'finished',
    package_unit: 'unit', description: null, metadata: {}, active: true,
    inventory: [], total_on_hand: 0,
  };
  await page.route(/\/api\/products\/SKU-A$/, (r) => json(r, product));
  const owners: ReadonlyArray<Employee> = ['emp-a', 'emp-z'].map((id) => ({
    id, name: id === 'emp-a' ? 'Ada Owner' : 'Zed Owner',
    email: null, role: null, department: null, skill_level: null,
    skills: [], hire_date: null, location: null, manager_id: null,
    employment_type: null, status: null, certifications: [],
  }));
  await page.route(/\/api\/people\/emp-[az]$/, (r) => {
    const id = new URL(r.request().url()).pathname.split('/').pop();
    const owner = owners.find((row) => row.id === id);
    if (!owner) throw new Error(`private owner fixture missing ${id}`);
    return json(r, owner);
  });
  await page.route(/\/api\/people\/accounts\/acc-a$/, (r) => json(r, AccountSchema.parse({
    id: 'acc-a', name: 'Linked account A', director: null, city: null,
    state: null, tier: null, customer_since: null, territory_rep_id: null,
  })));
  const campaignJob: Job = {
    id: '11111111-1111-4111-8111-00000000ca01', kind: 'ad-hoc',
    subject: { subject_kind: 'campaign', id: 'campaign-a' },
    title: 'Prepare linked campaign artwork', owner_id: 'emp-a',
    status: 'open', priority: 'standard', opened_on: '2026-09-02',
    due_on: null, closed_on: null, metadata: {}, tags: [], steps: [],
  };
  const campaignQueries: URLSearchParams[] = [];
  await page.route(/\/api\/jobs\?/, (r) => {
    const query = new URL(r.request().url()).searchParams;
    if (query.get('subject_id') === campaignJob.subject.id) {
      campaignQueries.push(query);
      return json(r, { data: [campaignJob], total: 1 });
    }
    return query.get('subject_id') === 'acc-a'
      ? json(r, { data: [], total: 0, limit: 500, offset: 0 })
      : r.fallback();
  });
  await page.route(/\/api\/(?:assets|commerce\/invoices|shipping\/shipments)\?/, (r) => {
    const query = new URL(r.request().url()).searchParams;
    return query.get('account_id') === 'acc-a'
      ? json(r, { data: [], total: 0, limit: Number(query.get('limit')), offset: 0 })
      : r.fallback();
  });
  const prev = asset('ma-previous', { title: 'Previous artwork' });
  const current = asset('ma-current', {
    ...A, id: 'ma-current', title: 'Current artwork', supersedes_id: prev.id,
    brand_reviewed_by: 'emp-a', brand_reviewed_at: '2026-09-05T00:00:00Z',
    file_url: 'https://files.example.invalid/artwork.svg', created_at: '2026-09-02T00:00:00Z',
  });
  const next = asset('ma-next', { title: 'Next artwork', created_at: '2026-09-03T00:00:00Z' });
  await page.route(DETAIL, (r) => {
    const id = new URL(r.request().url()).pathname.split('/').pop();
    return json(r, [prev, current, next].find((a) => a.id === id) ?? asset(id ?? 'missing'));
  });
  await page.route(HISTORY, (r) => json(r, [prev, current, next]));
  const target = `${PATH}/${current.id}`;
  await mountPage(page, target);
  await expect(detail(page).getByRole('heading')).toHaveText([
    'Current artwork', 'Profile', 'Tags (5)', 'Linked entities', 'SKUs (1)', 'Accounts (1)', 'Campaigns (1)', 'Version history (3)',
  ]);
  await expect(detail(page).locator('.detail-eyebrow')).toContainText('SUPERSEDED');
  await expect(profile(page).locator('dt')).toHaveText(['File', 'Owner', 'Brand-reviewed', 'Created', 'Supersedes']);
  await expect(cell(page, 'Owner')).toHaveText('Zed Owner');
  await expect(cell(page, 'Brand-reviewed')).toContainText('Ada Owner');
  await expect(detail(page).locator('table thead th')).toHaveText(['Version', 'ID', 'Title', 'Created', 'Status']);
  await expect(detail(page).locator('table tbody tr td:last-child')).toHaveText(['superseded', 'superseded', 'current']);
  await expect(detail(page).locator('button, form')).toHaveCount(0);
  // External file is the sole non-catalogue destination. Fulfil every
  // request to its private host so activation cannot reach a remote.
  const file = cell(page, 'File').getByRole('link');
  await expect(file).toHaveAttribute('href', current.file_url!);
  await expect(file).toHaveAttribute('target', '_blank');
  await expect(file).toHaveAttribute('rel', 'noopener noreferrer');
  const privateFiles: string[] = [];
  await page.context().route(/^https:\/\/files\.example\.invalid\//, (r) => {
    privateFiles.push(r.request().url());
    return r.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>Private asset file</title><h1>Private asset file</h1>' });
  });
  const opened = page.waitForEvent('popup');
  await file.click();
  const popup = await opened;
  await expect.poll(() => popup.url()).toBe(current.file_url);
  await expect(popup.getByRole('heading')).toHaveText('Private asset file');
  expect(privateFiles).toContain(current.file_url!);
  await popup.close();
  expect(new URL(page.url()).pathname).toBe(target);
  await expect(detail(page).locator('h1')).toHaveText(current.title);
  const links: ReadonlyArray<Readonly<{ label: string; href: string; kind: string }>> = [
    { label: '← Assets', href: PATH, kind: 'marketingAssets' },
    { label: current.id, href: target, kind: 'marketingAsset' },
    { label: 'Zed Owner', href: `${ROUTE_CATALOG.people.path}/emp-z`, kind: 'employee' },
    { label: 'Ada Owner', href: `${ROUTE_CATALOG.people.path}/emp-a`, kind: 'employee' },
    { label: prev.id, href: `${PATH}/${prev.id}`, kind: 'marketingAsset' },
    { label: 'SKU-A', href: `${ROUTE_CATALOG.products.path}/SKU-A`, kind: 'product' },
    { label: 'acc-a', href: `${ROUTE_CATALOG.accounts.path}/acc-a`, kind: 'account' },
    { label: 'campaign-a', href: `${ROUTE_CATALOG.jobs.path}?subject_kind=campaign&subject_id=campaign-a`, kind: 'jobs' },
    { label: next.id, href: `${PATH}/${next.id}`, kind: 'marketingAsset' },
  ];
  for (const link of links) {
    const anchor = detail(page).getByRole('link', { name: link.label, exact: true }).first();
    await expect(anchor).toHaveAttribute('href', link.href);
    const url = new URL(link.href, page.url());
    expect(parseRoute(url.pathname, url.search).kind).toBe(link.kind);
    await anchor.click();
    await expect.poll(() => new URL(page.url()).pathname + new URL(page.url()).search).toBe(link.href);
    if (link.kind === 'product') {
      await expect(page.getByRole('heading', { name: product.name, exact: true })).toBeVisible();
      await expect(page.getByRole('heading', { name: 'On-hand by location', exact: true })).toBeVisible();
      await expect(page.getByText('No inventory rows yet.', { exact: true })).toBeVisible();
    } else if (link.kind === 'employee') {
      await expect(page.locator('h1.detail-title')).toHaveText(link.label);
    } else if (link.kind === 'account') {
      await expect(page.getByRole('heading', { name: 'Linked account A', exact: true })).toBeVisible();
    } else if (link.kind === 'marketingAsset') {
      const destination = [prev, current, next].find((row) => `${PATH}/${row.id}` === link.href);
      if (!destination) throw new Error(`private asset fixture missing ${link.href}`);
      await expect(detail(page).locator('h1')).toHaveText(destination.title);
    } else if (link.kind === 'marketingAssets') {
      await expect(root(page).locator('h1')).toHaveText('Marketing assets (2)');
    } else if (link.kind === 'jobs') {
      await expect(page.getByRole('heading', { name: 'Filtered jobs', exact: true })).toBeVisible();
      await expect(page.getByRole('button', { name: 'Remove subject filter', exact: true }))
        .toHaveText('Subject id: campaign-a ✕');
      await expect.poll(() => campaignQueries.map((query) => Object.fromEntries(query)))
        .toEqual([{ status: 'open', subject_id: campaignJob.subject.id, limit: '200' }]);
      const campaignRow = root(page).locator('tbody tr').filter({ hasText: campaignJob.title });
      await expect(campaignRow).toHaveCount(1);
      await expect(campaignRow.getByRole('cell', { name: campaignJob.title, exact: true })).toHaveText(campaignJob.title);
      await expect(campaignRow.locator('td').first().getByRole('link'))
        .toHaveAttribute('href', `${ROUTE_CATALOG.jobs.path}/${campaignJob.id}`);
    }
    await page.goBack();
    await expect.poll(() => new URL(page.url()).pathname).toBe(target);
    await expect(detail(page).locator('h1')).toHaveText(current.title);
  }
  // The predecessor appears separately in Profile and version history;
  // exercising only the first would leave the history control unpinned.
  await detail(page).locator('table').getByRole('link', { name: prev.id, exact: true }).click();
  await expect.poll(() => new URL(page.url()).pathname).toBe(`${PATH}/${prev.id}`);
  await expect(detail(page).locator('h1')).toHaveText(prev.title);
  await page.goBack();
  await expect(detail(page).locator('h1')).toHaveText(current.title);
  expect(seen.writes).toEqual([]);
  expect(pageWrites(await openedRequests(page))).toEqual([]);
});

test('an original asset states genuine empty profile fields, unsafe file text and no curation controls', async ({ page }) => {
  const seen = await install(page);
  await page.route(DETAIL, (r) => json(r, asset('ma-empty', { file_url: 'javascript:alert(1)', retired_at: '2026-09-05T00:00:00Z' })));
  await mountPage(page, `${PATH}/ma-empty`);
  await expect(detail(page).locator('h1')).toHaveText('ma-empty');
  await expect(detail(page).locator('.detail-eyebrow')).toContainText('RETIRED');
  await expect(cell(page, 'File')).toHaveText('javascript:alert(1)');
  await expect(cell(page, 'File').locator('a')).toHaveCount(0);
  await expect(cell(page, 'Owner')).toHaveText('—');
  await expect(cell(page, 'Brand-reviewed')).toHaveText('not reviewed');
  await expect(cell(page, 'Supersedes')).toHaveText('(original)');
  await expect(detail(page)).toContainText('No tags yet.');
  await expect(detail(page).getByText('None.', { exact: true })).toHaveCount(3);
  await expect(detail(page)).toContainText('No prior versions — this is the original.');
  await expect(detail(page).locator('button, form')).toHaveCount(0);
  await expect(page.locator(FAILURE_MARKER)).toHaveCount(0);
  expect(seen.writes).toEqual([]);
});

test('detail 404 is absence, detail 503 is failure, and malformed history keeps the asset but refuses an original claim', async ({ page }) => {
  await install(page);
  await page.route(DETAIL, (r) => json(r, null, 404));
  await mountPage(page, `${PATH}/missing`);
  await expect(detail(page).locator('h1')).toHaveText('Asset not found');
  await expect(detail(page)).toContainText('No marketing asset with id missing.');
  await expect(detail(page).locator(FAILURE_MARKER)).toHaveCount(0);
  await page.unroute(DETAIL);
  await page.route(DETAIL, (r) => json(r, { error: 'catalog down' }, 503));
  await page.reload();
  await expect(detail(page).locator(FAILURE_MARKER)).toHaveText("Couldn't load this asset — HTTP 503");
  await expect(detail(page)).not.toContainText('Asset not found');
  await page.unroute(DETAIL);
  await page.route(DETAIL, (r) => json(r, asset('missing')));
  await page.route(HISTORY, (r) => json(r, { data: [] }));
  await page.reload();
  await expect(detail(page).locator('h1')).toHaveText('missing');
  await expect(detail(page).locator(FAILURE_MARKER)).toHaveText(
    "Couldn't load the version history — /api/catalog/marketing-assets/missing/history: the answer was not a list",
  );
  await expect(detail(page)).not.toContainText('No prior versions');
});
