// /ux/exec — the executive overview, rendered (page audit a1d62870).
//
// The crawls open the page with the floor's generic answers; this spec
// answers each read faithfully. Its reads, the line each read shows and
// each card's link are data in src/exec/exec.ts, pinned by
// src/exec/ExecPage.test.ts; this spec pins that the page PAINTS them:
//
//   * a refused jobs summary is a failure line carrying its HTTP status
//     on the shared marker, never a bare "unavailable" (backlog
//     1445813b), and a count says it is of the packets the viewer can
//     read (the summary is scoped to the caller since 19f08bd6);
//   * every card links to its deeper view, whatever its read answered
//     (c270e944);
//   * an empty balance sheet is a sentence (6d074e62);
//   * revenue is the ledger's income statement (d93cfb99), the count
//     waiting on the viewer links to My Day (f0f04375), and the
//     Executive department's own packets stand on the page (1f33e125);
//   * the retired reads are never made: the launch calendar and the
//     people read that named its owners (design 2ea444f5, a8991c86),
//     the brewery's products (6775546c) and commerce's summary.

import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG, departmentJobsPath } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.exec.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const card = (page: Page, title: string) =>
  page.locator('section.exec-card').filter({ has: page.locator('h2', { hasText: title }) });

type Answers = Readonly<{
  summary?: (r: Route) => Promise<void>;
  balance?: (r: Route) => Promise<void>;
  income?: (r: Route) => Promise<void>;
  waiting?: (r: Route) => Promise<void>;
  department?: (r: Route) => Promise<void>;
}>;

/// Every read the page makes, answered — the defaults are the smallest
/// honest ones — and every request path it sends, recorded.
async function install(page: Page, a: Answers = {}): Promise<string[]> {
  const asked: string[] = [];
  page.on('request', (req) => asked.push(new URL(req.url()).pathname));
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\/summary\?status=open$/, a.summary ?? ((r) =>
    json(r, { counts: { 'department-retro': 2, 'backlog-item': 5 }, total: 7, status: 'open' })));
  await page.route(/\/api\/workflows$/, (r) =>
    json(r, [{ kind: 'backlog-item', label: 'Backlog item' }]));
  await page.route(/\/api\/ledger\/balance-sheet$/, a.balance ?? ((r) =>
    json(r, {
      as_of: '2026-09-27',
      assets: [{ account_code: '1000', account_name: 'Cash', amount_cents: 100 }],
      liabilities: [],
      total_assets_cents: 100,
    })));
  await page.route(/\/api\/ledger\/income-statement\?/, a.income ?? ((r) =>
    json(r, {
      revenue: [{ account_code: '4200', account_name: 'Sponsorship revenue', amount_cents: 100 }],
      total_revenue_cents: 100,
    })));
  await page.route(/\/api\/jobs\/assignments\?for=me(&|$)/, a.waiting ?? ((r) =>
    json(r, { data: [{}, {}, {}], total: 3 })));
  await page.route(/\/api\/jobs\?(.*&)?department=executive(&|$)/, a.department ?? ((r) =>
    json(r, { data: [], total: 0 })));
  return asked;
}

test.describe('/ux/exec — the executive overview', () => {
  test('a refused jobs summary names its status on the failure marker (1445813b)', async ({ page }) => {
    await install(page, { summary: (r) => json(r, { error: 'denied' }, 403) });
    await mountPage(page, PATH);

    const failed = card(page, 'Active jobs').locator(FAILURE_MARKER);
    await expect(failed).toHaveText(
      "Couldn't load open jobs — /api/jobs/summary?status=open: HTTP 403",
    );
    await expect(failed).toHaveAttribute('role', 'alert');
  });

  test('the counts say whose they are, and name kinds by their labels', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);

    const jobs = card(page, 'Active jobs');
    await expect(jobs.locator('.stat-value').first()).toHaveText('7');
    await expect(jobs).toContainText('Counted over the packets you can read.');
    // A labelled kind by its label, an unlabelled one by its code.
    await expect(jobs.locator('.bar-label')).toHaveText(['Backlog item', 'department-retro']);
  });

  test('every card links to its deeper view, whatever its read answered (c270e944)', async ({ page }) => {
    const refuse = (r: Route) => json(r, { error: 'down' }, 503);
    await install(page, {
      summary: refuse, balance: refuse, income: refuse, waiting: refuse, department: refuse,
    });
    await mountPage(page, PATH);

    await expect(page.locator('section.exec-card')).toHaveCount(5);
    const links: ReadonlyArray<readonly [string, string, string]> = [
      ['Waiting on you', 'Open My Day →', '/ux/me'],
      ['Revenue mix', 'Open income statement →', '/ux/finance?tab=income-statement'],
      ['Cash & receivables', 'Open balance sheet →', '/ux/finance?tab=balance-sheet'],
      ['Active jobs', 'View all jobs →', '/ux/jobs'],
      ['Executive work', 'Open the Executive department →', departmentJobsPath('executive')],
    ];
    for (const [title, label, to] of links) {
      const c = card(page, title);
      await expect(c.getByRole('link', { name: label })).toHaveAttribute('href', to);
      // Each read's failure is its own line on the marker, never an
      // empty card.
      await expect(c.locator(FAILURE_MARKER)).toHaveCount(1);
    }
  });

  test('revenue is the ledger’s, by account, over the trailing year (d93cfb99)', async ({ page }) => {
    const asked = await install(page);
    await mountPage(page, PATH);

    const revenue = card(page, 'Revenue mix');
    await expect(revenue.locator('.mix-label')).toHaveText(['Sponsorship revenue']);
    await expect(revenue.locator('.mix-share')).toHaveText(['100%']);
    expect(asked).toContain('/api/ledger/income-statement');
  });

  test('an empty balance sheet is a sentence, not an empty row (6d074e62)', async ({ page }) => {
    await install(page, {
      balance: (r) => json(r, { as_of: '2026-09-27', assets: [], liabilities: [], total_assets_cents: 0 }),
    });
    await mountPage(page, PATH);

    await expect(card(page, 'Cash & receivables').locator('p.empty')).toHaveText(
      'No cash, receivable or payable balances on the ledger as of 2026-09-27.',
    );
  });

  test('what waits on the viewer is a count, not a second list (f0f04375)', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);

    await expect(card(page, 'Waiting on you')).toContainText('3 steps are waiting on you.');
  });

  test('the Executive department’s own packets stand on the page (1f33e125)', async ({ page }) => {
    await install(page, { department: (r) => json(r, { error: 'down' }, 500) });
    await mountPage(page, PATH);

    // The shared thirds component: a failed read is a failure, never
    // "nothing open".
    await expect(card(page, 'Executive work').locator(FAILURE_MARKER)).toContainText(
      "Couldn't load this department's jobs",
    );
  });

  test('the header promises what the cards show (faf3f635)', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);

    await expect(page.locator('.exec-subtitle')).toHaveText(
      'Money, open work, and what is waiting on you',
    );
  });

  test('the retired reads are never made', async ({ page }) => {
    const asked = await install(page);
    await mountPage(page, PATH);

    await expect(card(page, 'Active jobs').locator('.stat-value').first()).toHaveText('7');
    await expect(page.locator('section.exec-card').filter({ hasText: 'Finished goods' })).toHaveCount(0);
    await expect(page.locator('section.exec-card').filter({ hasText: 'Launches' })).toHaveCount(0);
    for (const retired of [
      '/api/jobs/launch-calendar', '/api/products', '/api/commerce/summary',
    ]) {
      expect(asked.filter((p) => p === retired), retired).toEqual([]);
    }
  });
});
