// Department routing derives from the registry (backlog 64656a46, car 2
// of design 8c3e9599, approved by David 2026-09-25).
//
// The pin the packet names: a registry fixture with an extra department
// renders its jobs view WITHOUT touching apps/web (its tab went with every
// department tab in backlog 474e2c1c: the top bar walks the station
// tree). `hosting` is a
// department on the live instance that owns no page in the catalog — the
// shape every department a company adds has on its first day — and
// nothing in apps/web names it. And the other half: a page whose owning
// department is not a row on this instance is the not-found page with
// one door Home, never a module-off notice for a department the company
// does not have (e543c6fe asked for exactly this).

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { DEPARTMENTS, DEPARTMENTS_ENDPOINT, installSmokeMocks, servePeopleRows } from './_smokeMocks';

const json = (r: Route, body: unknown): Promise<void> =>
  r.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

/// The smoke shell with the registry answering `roster` — registered
/// after installSmokeMocks, so this answer wins — signed in as someone
/// with a role, because the sidebar paints no group until the session
/// resolves one. Their own department is Finance.
async function withRoster(page: Page, roster: ReadonlyArray<Record<string, unknown>>): Promise<void> {
  await installSmokeMocks(page);
  await page.route(DEPARTMENTS_ENDPOINT, (r) => json(r, { data: roster, total: roster.length }));
  const emp = {
    id: 'emp-ops', name: 'Ops', email: 'o@a', role: 'ceo', department: 'finance',
    hire_date: '2023-01-01', status: 'active', location: 'HQ', employment_type: 'full-time',
    skills: [], certifications: [],
  };
  await page.route(/\/api\/people$/, (r) => json(r, [emp]));
  await servePeopleRows(page, [emp]);
  await page.route(/\/api\/session$/, (r) => json(r, { username: 'ops', employee_id: emp.id, role: emp.role }));
}

const HOSTING = { code: 'hosting', display_name: 'Hosting', function: 'revenue' };

test('a department the registry adds keeps its jobs view at /<code>, and draws no tab and no sidebar row', async ({ page }) => {
  await withRoster(page, [...DEPARTMENTS, HOSTING]);
  const reads: string[] = [];
  page.on('request', (req) => {
    if (/\/api\/jobs\?(.*&)?department=hosting(&|$)/.test(req.url())) reads.push(req.url());
  });
  await mountPage(page, '/hosting');

  // The bars are the station tree's since backlog 474e2c1c (David
  // 2026-11-19): a department that is no station has no tab — the tab per
  // registry row drew departments the company no longer has — and no
  // Jobs row. Its page stands at the company root, as a record view does.
  await expect(page.locator('.perspective-tabs .perspective-tab')).toHaveCount(0);
  await expect(page.locator('.perspective-tabs a[href="/hosting"]')).toHaveCount(0);
  await expect(page.locator('.perspective-tabs [aria-current="page"]')).toHaveAttribute('data-trail-stop', 'company');
  await expect(page.locator('.shell-sidebar a[href="/hosting/jobs"]')).toHaveCount(0);

  // The page: the department's own in / working / out, read by its code.
  await expect(page.locator('h1')).toHaveText('Jobs');
  await expect.poll(() => reads.length).toBeGreaterThan(0);

  // /hosting/jobs is the same page.
  await mountPage(page, '/hosting/jobs');
  await expect(page.locator('h1')).toHaveText('Jobs');
});

// Pinned on the Service queue (/ux/service) until it went with step 1 of
// design 01fa5bdf; the Churn watchlist, owned by Sales, carries it now.
test('a page owned by a department this instance does not have is not found, with one door Home', async ({ page }) => {
  await withRoster(page, DEPARTMENTS.filter((d) => d.code !== 'sales'));
  await mountPage(page, '/watchlist');

  await expect(page.getByText('Sales is not a department on this instance')).toBeVisible();
  await expect(page.locator('code', { hasText: '/watchlist' })).toBeVisible();
  const back = page.getByRole('link', { name: 'Back to Home' });
  await expect(back).toHaveAttribute('href', '/');
  // Never the module-off notice, and never the watchlist.
  await expect(page.getByText('Not enabled for this tenant')).toHaveCount(0);
  await expect(page.getByText('Churn watchlist')).toHaveCount(0);
  // No department is shown as the reader's place: the bar stands at the
  // company root, where the door leads.
  await expect(page.locator('.perspective-tabs [aria-current="page"]')).toHaveAttribute('data-trail-stop', 'company');
});

test('a department the instance has renders its page as it always did', async ({ page }) => {
  // The control for the test above: the same path, the full roster.
  await withRoster(page, DEPARTMENTS);
  await mountPage(page, '/watchlist');
  await expect(page.getByText('is not a department on this instance')).toHaveCount(0);
  await expect(page.getByText('Churn watchlist').first()).toBeVisible();
});

test('the retired /ux/departments/<code> is not found, like any path nothing serves', async ({ page }) => {
  await withRoster(page, DEPARTMENTS);
  await mountPage(page, '/ux/departments/sales');
  await expect(page.getByText('Nothing in this app answers')).toBeVisible();
  await expect(page.locator('code', { hasText: '/ux/departments/sales' })).toBeVisible();
});
