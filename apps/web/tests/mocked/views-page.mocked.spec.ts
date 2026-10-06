// /ux/views — every failure path the page has, pinned with a SIGNED-IN
// session (page audit d2e86594, 2026-09-27; backlog daba5702).
//
// Before this spec, 0 of the page's 5 failure paths was pinned. The
// outage crawl exempted /ux/views because the empty mocked session
// never fires /api/views; the same session disabled Save and listed no
// View, so the interaction crawl never reached Run or Delete; and there
// was no test under src/views/. A failure line nothing pins is a
// failure line that can silently go.
//
// Pinned here, each against the backend answer that provokes it:
//   the list read failing            — the list's own failure line
//   a Run failing                    — that View's failure line
//   a Save refused 422               — against the form (0148c461)
//   a Delete refused                 — on that View's row; the list stays (0148c461)
//   a weak truncation                — names the View's own source (3d8178e1)
// and beside them the audit's other decisions on this page: a session
// with no viewer says so instead of loading forever (04754a40), a
// read-only session is offered no write (14ef5e06), result ids link to
// what they name and the sharer is named (00ab7ddd), Edit PUTs the
// View (c4f3c53a), a result says what it shows and when (22d5bfcd),
// and the composer's examples run on any instance (6d4eea00).
//
// The denied-source line the audit also named (gap 1, backlog 5392cf23)
// needs a server that says a source was denied; it rides with that
// car, not this one.

import { expect, test, type Page, type Route } from './_test';
import { installApiFloor, servePeopleRows } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.views.path;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const person = (id: string, name: string) => ({
  id, name, email: `${id}@a`, role: 'platform-admin', department: 'it',
  hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
});
const ME = person('emp-001', 'David');
const SHARER = person('emp-002', 'Robin');

const view = (over: Record<string, unknown>) => ({
  id: 'view-1', owner_id: ME.id, title: 'Open jobs', source: 'jobs',
  filter: 'status = "open"', columns: ['id', 'status'], layout: 'table',
  visibility: 'private', created_at: '2026-09-01T00:00:00Z',
  updated_at: '2026-09-01T00:00:00Z', ...over,
});
const MINE = view({});
const SHARED = view({
  id: 'view-2', owner_id: SHARER.id, title: 'Recent steps', source: 'steps',
  filter: '', columns: [], visibility: 'shared',
});

const LIST = /\/api\/views$/;
const ONE = (id: string) => new RegExp(`/api/views/${id}$`);
const RESULTS = (id: string) => new RegExp(`/api/views/${id}/results`);

type Session = 'operator' | 'guest' | 'nobody';

/// The floor, a session, and a two-View list (one mine, one shared).
async function viewsMocks(page: Page, who: Session = 'operator', views: unknown[] = [MINE, SHARED]): Promise<void> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await installApiFloor(page);
  await servePeopleRows(page, [ME, SHARER]);
  const probe =
    who === 'operator' ? { username: 'david', employee_id: ME.id, role: 'platform-admin' }
      : who === 'guest' ? { username: 'guest', role: 'visitor' }
        : {};
  await page.route(/\/api\/session$/, (r) => json(r, probe));
  await page.route(LIST, (r) => json(r, views));
}

async function mount(page: Page): Promise<void> {
  await page.goto(PATH);
  await expect(page.getByRole('heading', { name: 'Views', level: 1 })).toBeVisible();
}

const row = (page: Page, id: string) => page.locator(`[data-view="${id}"]`);

// ---- the five failure paths -------------------------------------------

test('a failed list read says the list failed, with the marker', async ({ page }) => {
  await viewsMocks(page);
  await page.route(LIST, (r) => r.fulfill({ status: 500, body: 'down' }));
  await mount(page);
  const failed = page.locator('.v-list-failed');
  await expect(failed).toHaveClass(/load-failed/);
  await expect(failed).toHaveAttribute('role', 'alert');
  await expect(failed).toContainText('views: HTTP 500');
});

test('a failed Run says so on that View, and the other View is untouched', async ({ page }) => {
  await viewsMocks(page);
  await page.route(RESULTS('view-1'), (r) => r.fulfill({ status: 403, body: 'no read on jobs' }));
  await mount(page);
  await row(page, 'view-1').getByRole('button', { name: 'Run' }).click();
  const failed = row(page, 'view-1').locator('.load-failed[role="alert"]');
  await expect(failed).toContainText('HTTP 403: no read on jobs');
  await expect(row(page, 'view-2').locator('.load-failed')).toHaveCount(0);
});

test('a Save refused 422 is shown against the form, as a refusal', async ({ page }) => {
  await viewsMocks(page);
  await page.route(LIST, (r) =>
    r.request().method() === 'POST'
      ? r.fulfill({ status: 422, body: 'invalid filter: unexpected token at 7' })
      : json(r, [MINE, SHARED]));
  await mount(page);
  await page.getByLabel('Title').fill('Broken');
  await page.getByRole('textbox', { name: 'Filter optional' }).fill('status ==');
  await page.getByRole('button', { name: 'Save view' }).click();
  const refused = page.locator('.v-actions .load-failed[role="alert"]');
  await expect(refused).toHaveText('save refused: HTTP 422, invalid filter: unexpected token at 7');
  // The draft stays, so the typo can be fixed in place.
  await expect(page.getByRole('textbox', { name: 'Filter optional' })).toHaveValue('status ==');
});

test('a refused Delete is said on its own row, and the list stays', async ({ page }) => {
  await viewsMocks(page);
  await page.route(ONE('view-1'), (r) =>
    r.request().method() === 'DELETE' ? r.fulfill({ status: 403, body: 'not your view' }) : r.fallback());
  await mount(page);
  await row(page, 'view-1').getByRole('button', { name: 'Delete' }).click();
  await expect(row(page, 'view-1').locator('.load-failed[role="alert"]')).toHaveText(
    'delete refused: HTTP 403, not your view',
  );
  // Both Views are still listed, and nothing claims the LIST failed.
  await expect(row(page, 'view-1')).toBeVisible();
  await expect(row(page, 'view-2')).toBeVisible();
  await expect(page.locator('.v-list-failed')).toHaveCount(0);
});

test('a weak truncation names the View\'s own rows, not "events"', async ({ page }) => {
  await viewsMocks(page);
  await page.route(RESULTS('view-1'), (r) =>
    json(r, { view_id: 'view-1', source: 'jobs', layout: 'table', rows: [{ id: 'j-1', status: 'open' }],
      matched: 1, pushed_down: 0, truncated: true, scope: 'all' }));
  await mount(page);
  await row(page, 'view-1').getByRole('button', { name: 'Run' }).click();
  const trunc = row(page, 'view-1').locator('.v-trunc');
  await expect(trunc).toContainText('only the newest 5,000 jobs were examined');
  await expect(trunc).not.toContainText('events');
});

// ---- what a result says -------------------------------------------------

test('a result says how many rows it shows of how many matched, and when it was read', async ({ page }) => {
  await viewsMocks(page);
  await page.route(RESULTS('view-1'), (r) =>
    json(r, { view_id: 'view-1', source: 'jobs', layout: 'table',
      rows: [{ id: 'j-1', status: 'open' }, { id: 'j-2', status: 'open' }],
      matched: 4000, pushed_down: 1, truncated: false, scope: 'all' }));
  await mount(page);
  await row(page, 'view-1').getByRole('button', { name: 'Run' }).click();
  await expect(row(page, 'view-1').locator('.v-shown')).toHaveText(/^showing 2 of 4000 · read \d\d:\d\d$/);
});

test('result ids link to what they name', async ({ page }) => {
  await viewsMocks(page);
  await page.route(RESULTS('view-1'), (r) =>
    json(r, { view_id: 'view-1', source: 'jobs', layout: 'table', rows: [{ id: 'j-1', status: 'open' }],
      matched: 1, pushed_down: 1, truncated: false, scope: 'all' }));
  await page.route(RESULTS('view-2'), (r) =>
    json(r, { view_id: 'view-2', source: 'steps', layout: 'table', rows: [{ id: 's-1', job_id: 'j-1', status: 'ready' }],
      matched: 1, pushed_down: 0, truncated: false, scope: 'all' }));
  await mount(page);
  await row(page, 'view-1').getByRole('button', { name: 'Run' }).click();
  await expect(row(page, 'view-1').getByRole('link', { name: 'j-1' })).toHaveAttribute('href', '/ux/jobs/j-1');
  await row(page, 'view-2').getByRole('button', { name: 'Run' }).click();
  await expect(row(page, 'view-2').getByRole('link', { name: 's-1' })).toHaveAttribute(
    'href', '/ux/jobs/j-1/steps/s-1?from=%2Fux%2Fviews&from_label=Views');
  await expect(row(page, 'view-2').getByRole('link', { name: 'j-1' })).toHaveAttribute('href', '/ux/jobs/j-1');
  // Plain values stay text.
  await expect(row(page, 'view-1').getByRole('link', { name: 'open' })).toHaveCount(0);
});

test('a shared View names its sharer from their people row', async ({ page }) => {
  await viewsMocks(page);
  await mount(page);
  await expect(row(page, 'view-2').getByText('shared by Robin')).toBeVisible();
  await expect(row(page, 'view-2').getByText('shared by emp-002')).toHaveCount(0);
});

// ---- the session ---------------------------------------------------------

test('a session with no viewer says there is none, instead of loading forever', async ({ page }) => {
  await viewsMocks(page, 'nobody');
  await mount(page);
  await expect(page.getByText('No signed-in viewer, so no Views to list.')).toBeVisible();
  await expect(page.getByText('Loading views…')).toHaveCount(0);
});

test('a read-only session is offered no write', async ({ page }) => {
  await viewsMocks(page, 'guest', [view({ owner_id: 'guest' }), SHARED]);
  await mount(page);
  await expect(row(page, 'view-2')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Save view' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Delete' })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Edit' })).toHaveCount(0);
  // Run is a read, and stays.
  await expect(row(page, 'view-2').getByRole('button', { name: 'Run' })).toBeVisible();
});

// ---- the composer ----------------------------------------------------------

test('Edit loads the View into the composer and PUTs it', async ({ page }) => {
  await viewsMocks(page);
  const seen: { put?: { url: string; body: unknown } } = {};
  await page.route(ONE('view-1'), (r) => {
    if (r.request().method() !== 'PUT') return r.fallback();
    seen.put = { url: r.request().url(), body: r.request().postDataJSON() };
    return json(r, { ...MINE, title: 'Open jobs, fixed' });
  });
  await mount(page);
  await row(page, 'view-1').getByRole('button', { name: 'Edit' }).click();
  await expect(page.getByLabel('Title')).toHaveValue('Open jobs');
  await expect(page.getByRole('textbox', { name: 'Filter optional' })).toHaveValue('status = "open"');
  await page.getByLabel('Title').fill('Open jobs, fixed');
  await page.getByRole('button', { name: 'Save changes' }).click();
  await expect.poll(() => seen.put).toBeDefined();
  expect(new URL(seen.put!.url).pathname).toBe('/api/views/view-1');
  expect(seen.put!.body).toEqual({
    title: 'Open jobs, fixed', source: 'jobs', filter: 'status = "open"',
    columns: ['id', 'status'], layout: 'table', visibility: 'private',
  });
  // Back to a new View once the edit landed.
  await expect(page.getByRole('button', { name: 'Save view' })).toBeVisible();
});

test("the composer's examples run on any instance", async ({ page }) => {
  await viewsMocks(page);
  await mount(page);
  await expect(page.getByLabel('Title')).toHaveAttribute('placeholder', 'Open backlog items');
  await expect(page.getByRole('textbox', { name: 'Filter optional' })).toHaveAttribute('placeholder', 'status = "open" AND kind = "backlog-item"');
});
