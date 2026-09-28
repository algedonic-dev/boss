// /ux/views reads what its sources offer from the server, and says
// whose rows a result is (page audit d2e86594, 2026-09-27).
//
//   backlog 4a8939b5 — the fields, the pushable names and the scan
//     ceiling lived as three client copies, and two had drifted: the
//     events picker could not offer subject_kind / subject_id, and the
//     pushdown hint omitted payload.<path>. The page now reads
//     GET /api/views/sources; these pin that it offers what that serves
//     and says so when it cannot be read.
//   backlog 2b5ad29a — jobs and steps carry `metadata`, jobs `partition`.
//   backlog 5392cf23 — a source the viewer may not read is a 403 naming
//     the policy resource, shown on that View's line with the failure
//     marker (the denied-source line the ViewsPage UI car deferred to
//     this one); an owner-scoped count says it is only the viewer's rows.

import { expect, test, type Page, type Route } from './_test';
import { VIEW_SOURCES, VIEW_SOURCES_FIXTURE, installApiFloor, servePeopleRows } from './_smokeMocks';
import { ROUTE_CATALOG } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.views.path;

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const ME = {
  id: 'emp-001', name: 'David', email: 'emp-001@a', role: 'platform-admin', department: 'it',
  hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const view = (over: Record<string, unknown>) => ({
  id: 'view-1', owner_id: ME.id, title: 'Open jobs', source: 'jobs',
  filter: '', columns: [], layout: 'count', visibility: 'private',
  created_at: '2026-09-01T00:00:00Z', updated_at: '2026-09-01T00:00:00Z', ...over,
});

const results = (over: Record<string, unknown>) => ({
  view_id: 'view-1', source: 'jobs', layout: 'count', rows: [], matched: 3,
  pushed_down: 0, truncated: false, scope: 'all', ...over,
});

/// The floor (which serves the sources fixture), a signed-in viewer,
/// and one View of theirs.
async function viewsMocks(page: Page, views: unknown[] = [view({})]): Promise<void> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
  });
  await installApiFloor(page);
  await servePeopleRows(page, [ME]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: ME.id, role: 'platform-admin' }));
  await page.route(/\/api\/views$/, (r) => json(r, views));
}

async function mount(page: Page): Promise<void> {
  await page.goto(PATH);
  await expect(page.getByRole('heading', { name: 'Views', level: 1 })).toBeVisible();
}

async function run(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Run' }).click();
}

// ---- 4a8939b5 / 2b5ad29a: the picker offers what the server serves --------

test('the events picker offers the subject fields the client copy dropped', async ({ page }) => {
  await viewsMocks(page);
  await mount(page);
  await page.getByLabel('Source').selectOption('events');
  const chips = page.locator('.v-chips');
  await expect(chips.getByRole('button', { name: 'subject_kind' })).toBeVisible();
  await expect(chips.getByRole('button', { name: 'subject_id' })).toBeVisible();
});

test('the jobs picker offers metadata and partition', async ({ page }) => {
  await viewsMocks(page);
  await mount(page);
  const chips = page.locator('.v-chips');
  await expect(chips.getByRole('button', { name: 'metadata' })).toBeVisible();
  await expect(chips.getByRole('button', { name: 'partition' })).toBeVisible();
});

test('the picker offers exactly what the server serves, not a copy of it', async ({ page }) => {
  await viewsMocks(page);
  await page.route(VIEW_SOURCES, (r) =>
    json(r, { scan_ceiling: 5000, sources: [{ source: 'jobs', fields: ['only_this'], pushable: [] }] }));
  await mount(page);
  await expect(page.locator('.v-chips').getByRole('button')).toHaveText(['only_this']);
});

test('sources that cannot be read say so, with the marker, instead of an empty picker', async ({ page }) => {
  await viewsMocks(page);
  await page.route(VIEW_SOURCES, (r) => r.fulfill({ status: 500, body: 'down' }));
  await mount(page);
  const failed = page.locator('.v-field .load-failed[role="alert"]');
  await expect(failed).toContainText('could not be read');
  await expect(failed).toContainText('HTTP 500');
  await expect(page.locator('.v-chips')).toHaveCount(0);
});

test('the weak-truncation hint names the served ceiling and payload.<path>', async ({ page }) => {
  await viewsMocks(page, [view({ source: 'events', filter: 'payload.sku = "FP-1" OR kind = "x"' })]);
  await page.route(/\/api\/views\/view-1\/results/, (r) =>
    json(r, results({ source: 'events', matched: 0, truncated: true, pushed_down: 0 })));
  await mount(page);
  await run(page);
  const trunc = page.locator('.v-trunc');
  await expect(trunc).toContainText(`newest ${VIEW_SOURCES_FIXTURE.scan_ceiling.toLocaleString('en-US')}`);
  await expect(trunc).toContainText('payload.<path>');
  await expect(trunc).toContainText('subject_kind, subject_id');
});

// ---- 5392cf23: whose rows, and a refusal that is not "0 matches" --------

test('an owner-scoped count says it is only the rows the viewer may read', async ({ page }) => {
  await viewsMocks(page);
  await page.route(/\/api\/views\/view-1\/results/, (r) => json(r, results({ scope: 'owners' })));
  await mount(page);
  await run(page);
  await expect(page.locator('.v-count')).toContainText('3 matches');
  await expect(page.locator('.v-scope')).toHaveText('— only rows you may read');
});

test('a count over every row carries no scope line', async ({ page }) => {
  await viewsMocks(page);
  await page.route(/\/api\/views\/view-1\/results/, (r) => json(r, results({ scope: 'all' })));
  await mount(page);
  await run(page);
  await expect(page.locator('.v-count')).toContainText('3 matches');
  await expect(page.locator('.v-scope')).toHaveCount(0);
});

test('a denied source is a refusal naming its resource, never "0 matches"', async ({ page }) => {
  await viewsMocks(page);
  const refusal =
    'view source jobs refused: role guest holds no read of policy resource job that this source can apply';
  await page.route(/\/api\/views\/view-1\/results/, (r) => r.fulfill({ status: 403, body: refusal }));
  await mount(page);
  await run(page);
  const failed = page.locator('.load-failed[role="alert"]');
  await expect(failed).toContainText('HTTP 403');
  await expect(failed).toContainText('policy resource job');
  await expect(page.locator('.v-count')).toHaveCount(0);
});
