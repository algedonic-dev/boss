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
import { mountPage, settledReads } from './_helpers';
import { installSmokeMocks } from './_smokeMocks';
import { FAILURE_MARKER } from './_routes';
import { ROUTE_CATALOG, departmentJobsPath } from '../../src/shell/nav-catalog';

const PATH = ROUTE_CATALOG.exec.path;

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const card = (page: Page, title: string) =>
  page.locator('section.exec-card').filter({ has: page.locator('h2', { hasText: title }) });

// Listed jobs carry slim steps; the detail read carries the full step.
// Both are native shapes, rather than a title-only stub that would hide
// a broken row link on arrival.
const packet = (id: string, status: 'open' | 'closed', stepStatus: 'ready' | 'active' | 'completed') => ({
  id, kind: 'department-retro', title: `Executive packet ${id.slice(0, 8)}`,
  status, priority: 'standard', subject: { subject_kind: 'custom', id: 'executive-subject' },
  owner_id: null, opened_on: '2026-09-21', due_on: null,
  closed_on: status === 'closed' ? '2026-09-27' : null, metadata: {}, tags: [],
  steps: [{ id: `${id}-step`, kind: 'task', title: 'Review the packet', status: stepStatus, sort_order: 0 }],
});
const EXECUTIVE_PACKETS = [
  packet('11111111-0000-0000-0000-000000000001', 'open', 'ready'),
  packet('22222222-0000-0000-0000-000000000002', 'open', 'active'),
  packet('33333333-0000-0000-0000-000000000003', 'closed', 'completed'),
];
const departmentAnswer = (r: Route, totalExtra = 0) => {
  const terminal = new URL(r.request().url()).searchParams.get('terminal') === 'true';
  const rows = EXECUTIVE_PACKETS.filter((j) => (j.status === 'closed') === terminal);
  return json(r, { data: rows, total: rows.length + totalExtra });
};

type Answers = Readonly<{
  summary?: (r: Route) => Promise<void>;
  balance?: (r: Route) => Promise<void>;
  income?: (r: Route) => Promise<void>;
  waiting?: (r: Route) => Promise<void>;
  department?: (r: Route) => Promise<void>;
  workflows?: (r: Route) => Promise<void>;
  ages?: (r: Route) => Promise<void>;
}>;

/// Every read the page makes, answered — the defaults are the smallest
/// honest ones — and every request path it sends, recorded.
async function install(page: Page, a: Answers = {}): Promise<string[]> {
  const asked: string[] = [];
  page.on('request', (req) => asked.push(new URL(req.url()).pathname));
  await installSmokeMocks(page);
  await page.route(/\/api\/jobs\/summary\?status=open$/, a.summary ?? ((r) =>
    json(r, { counts: { 'department-retro': 2, 'backlog-item': 5 }, total: 7, status: 'open' })));
  await page.route(/\/api\/workflows$/, a.workflows ?? ((r) =>
    json(r, [{ kind: 'backlog-item', label: 'Backlog item' }])));
  await page.route(/\/api\/ledger\/balance-sheet$/, a.balance ?? ((r) =>
    json(r, {
      as_of: '2026-09-27',
      assets: [{ account_code: '1000', account_name: 'Cash', amount_cents: 100 }],
      liabilities: [],
      total_assets_cents: 100,
      equity: [{ account_code: '3000', account_name: 'Equity', amount_cents: 100 }],
      total_liabilities_cents: 0, total_equity_cents: 100, imbalance_cents: 0, balanced: true, currency: 'USD',
    })));
  await page.route(/\/api\/ledger\/income-statement\?/, a.income ?? ((r) =>
    json(r, {
      revenue: [{ account_code: '4200', account_name: 'Sponsorship revenue', amount_cents: 100 }],
      total_revenue_cents: 100,
      from: new URL(r.request().url()).searchParams.get('from'),
      to: new URL(r.request().url()).searchParams.get('to'),
      cogs: [], total_cogs_cents: 0, gross_profit_cents: 100,
      operating_expenses: [], total_operating_expenses_cents: 0, net_income_cents: 100, currency: 'USD',
    })));
  await page.route(/\/api\/jobs\/assignments\?for=me(&|$)/, a.waiting ?? ((r) =>
    json(r, { data: [{}, {}, {}], total: 3 })));
  await page.route(/\/api\/jobs\?(.*&)?department=executive(&|$)/, a.department ?? ((r) =>
    json(r, { data: [], total: 0 })));
  await page.route(/\/api\/jobs\/queue-age$/, a.ages ?? ((r) => json(r, { data: [] })));
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

test.describe('the complete current Executive controls', () => {
  test('each of the five card links navigates to its catalogued deeper view and Back restores Exec', async ({ page }) => {
    await install(page);
    await mountPage(page, PATH);
    const links: ReadonlyArray<readonly [string, string, string]> = [
      // My Day is an intentional inline shell route, not a module
      // row in the catalog; existing router/unit pins hold /ux/me.
      ['Waiting on you', 'Open My Day →', '/ux/me'],
      ['Revenue mix', 'Open income statement →', `${ROUTE_CATALOG.finance.path}?tab=income-statement`],
      ['Cash & receivables', 'Open balance sheet →', `${ROUTE_CATALOG.finance.path}?tab=balance-sheet`],
      ['Active jobs', 'View all jobs →', ROUTE_CATALOG.jobs.path],
      ['Executive work', 'Open the Executive department →', departmentJobsPath('executive')],
    ];
    for (const [title, label, target] of links) {
      await card(page, title).getByRole('link', { name: label, exact: true }).click();
      await expect(page).toHaveURL(new RegExp(`${target.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
      await expect(page.locator('.exec')).toHaveCount(0);
      await page.goBack();
      await expect(page.locator('.exec-subtitle')).toHaveText('Money, open work, and what is waiting on you');
      await expect(page.locator('section.exec-card')).toHaveCount(5);
    }
  });

  test('true empty answers paint five explicit empty cards, with links and no writes or forms', async ({ page }) => {
    const writes: string[] = [];
    page.on('request', (request) => {
      if (new URL(request.url()).pathname.startsWith('/api/') && !['GET', 'HEAD'].includes(request.method())) {
        writes.push(`${request.method()} ${new URL(request.url()).pathname}`);
      }
    });
    await install(page, {
      summary: (r) => json(r, { counts: {}, total: 0 }),
      income: (r) => json(r, { revenue: [], total_revenue_cents: 0 }),
      balance: (r) => json(r, { as_of: '2026-09-27', assets: [], liabilities: [] }),
      waiting: (r) => json(r, { data: [], total: 0 }),
    });
    await mountPage(page, PATH);
    await expect(card(page, 'Waiting on you')).toContainText('Nothing is waiting on you.');
    await expect(card(page, 'Revenue mix')).toContainText('No revenue posted to the ledger between');
    await expect(card(page, 'Cash & receivables')).toContainText('No cash, receivable or payable balances on the ledger as of 2026-09-27.');
    await expect(card(page, 'Active jobs')).toContainText('No open jobs among the packets you can read.');
    await expect(card(page, 'Executive work')).toContainText('No jobs in Executive:');
    await expect(page.locator('.exec').locator(FAILURE_MARKER)).toHaveCount(0);
    await expect(page.locator('.exec a')).toHaveCount(5);
    await expect(page.locator('.exec button, .exec form')).toHaveCount(0);
    // The shell records entry into an existing surface; the page
    // sends no business command. Any other non-GET is a regression.
    expect(writes).toEqual(['POST /api/surface-opens']);
  });

  for (const [field, title, body] of [
    ['summary', 'Active jobs', { counts: { unread: 'unknown' }, total: 0 }],
    ['income', 'Revenue mix', { revenue: [null], total_revenue_cents: 0 }],
    ['balance', 'Cash & receivables', { as_of: '2026-09-27', assets: [] }],
    ['waiting', 'Waiting on you', { data: [], total: 'unread' }],
    ['department', 'Executive work', { data: null, total: 0 }],
  ] as const) {
    test(`malformed successful ${field} answer is a failed read, never an empty card`, async ({ page }) => {
      await install(page, { [field]: (r: Route) => json(r, body) });
      await mountPage(page, PATH);
      await expect(card(page, title).locator(FAILURE_MARKER)).toHaveCount(1);
      await expect(card(page, title).locator(FAILURE_MARKER)).toContainText("Couldn't load");
    });
  }

  test('a refused workflow-label read preserves the counts and falls back to actual kind codes', async ({ page }) => {
    await install(page, { workflows: (r) => json(r, { error: 'denied' }, 403) });
    await mountPage(page, PATH);
    await expect(card(page, 'Active jobs').locator('.bar-label')).toHaveText(['backlog-item', 'department-retro']);
    await expect(card(page, 'Active jobs').locator('.stat-value').first()).toHaveText('7');
  });

  test('loading remains distinct from empty on every card, and the links remain available', async ({ page }) => {
    let release: () => void = () => {};
    const pending = new Promise<void>((resolve) => { release = resolve; });
    const delayed = async (r: Route) => {
      await pending;
      return json(r, {});
    };
    await install(page, {
      summary: delayed, income: delayed, balance: delayed, waiting: delayed, department: delayed,
    });
    await mountPage(page, PATH);
    try {
      for (const title of ['Waiting on you', 'Revenue mix', 'Cash & receivables', 'Active jobs', 'Executive work']) {
        await expect(card(page, title)).toContainText('Loading');
        await expect(card(page, title).locator('.card-link a')).toHaveCount(1);
      }
      await expect(page.locator('.exec').locator(FAILURE_MARKER)).toHaveCount(0);
    } finally {
      release();
    }
  });

  for (const read of [
    { key: 'summary', title: 'Active jobs' },
    { key: 'income', title: 'Revenue mix' },
    { key: 'balance', title: 'Cash & receivables' },
    { key: 'waiting', title: 'Waiting on you' },
    { key: 'department', title: 'Executive work', terminal: false },
    { key: 'department', title: 'Executive work', terminal: true },
    { key: 'ages', title: 'Executive work' },
    { key: 'workflows', title: 'Active jobs' },
  ] as const) {
    for (const failure of ['HTTP 403', 'network'] as const) {
      test(`${read.key}${'terminal' in read ? ` terminal=${read.terminal}` : ''}: ${failure} never becomes healthy empty`, async ({ page }) => {
        const refuse = (r: Route) => failure === 'network' ? r.abort('failed') : json(r, { error: 'denied' }, 403);
        const answer = (r: Route) => 'terminal' in read
          && (new URL(r.request().url()).searchParams.get('terminal') === 'true') !== read.terminal
            ? departmentAnswer(r) : refuse(r);
        await install(page, { department: (r) => departmentAnswer(r), [read.key]: answer });
        await mountPage(page, PATH);
        const panel = card(page, read.title);
        if (read.key === 'workflows') {
          // Labels are auxiliary: a dark label registry cannot discard
          // the independently read counts or invent names for the codes.
          await expect(panel.locator('.stat-value').first()).toHaveText('7');
          await expect(panel.locator('.bar-label')).toHaveText(['backlog-item', 'department-retro']);
        } else {
          await expect(panel.locator(FAILURE_MARKER)).toHaveCount(1);
          await expect(panel.locator(FAILURE_MARKER)).toContainText(failure === 'network' ? 'Failed to fetch' : 'HTTP 403');
          if (read.key === 'ages') {
            await expect(panel.locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
            await expect(panel.locator('td.waiting-for')).toHaveText(['unreadable', 'unreadable', '']);
          }
        }
        await expect(panel.locator('.card-link a')).toHaveCount(1);
      });
    }
  }

  test('counted blank department pages are unread, never a claim that no jobs exist', async ({ page }) => {
    await install(page, { department: (r) => json(r, { data: [], total: 1 }) });
    await mountPage(page, PATH);
    await expect(card(page, 'Executive work').locator(FAILURE_MARKER)).toContainText('invalid counted envelope');
    await expect(card(page, 'Executive work')).not.toContainText('No jobs in Executive');
  });

  test('three thirds retain counts, truncation, step ages and every row control through click, keyboard and Back', async ({ page }) => {
    const asked = await install(page, {
      department: (r) => departmentAnswer(r, 2),
      ages: (r) => json(r, {
        data: EXECUTIVE_PACKETS.slice(0, 2).map((j, i) => ({
          job_id: j.id, step_id: j.steps[0]!.id, since: '2026-09-27T10:00:00Z', exact: i === 0,
          waiting_seconds: 7200, waiting_days: 7200 / 86400,
        })), total: 2, now: '2026-09-27T12:00:00Z',
      }),
    });
    for (const j of EXECUTIVE_PACKETS) {
      await page.route(`**/api/jobs/${j.id}`, (r) => json(r, {
        ...j, steps: j.steps.map((s) => ({
          ...s, job_id: j.id, assignee_id: null, blocked_by: [], metadata: {}, fields: [], completed_on: null,
        })),
      }));
    }
    await mountPage(page, PATH);
    const panel = card(page, 'Executive work');
    await expect(panel.locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
    await expect(panel.locator('.truncated')).toHaveText([
      'Showing 2 of 4 live packets (In and Working) — that read is one page; its thirds below are of the page, not of the department.',
      'Showing 1 of 3 departures (Out) — that read is one page; its thirds below are of the page, not of the department.',
    ]);
    await expect(panel.locator('td.waiting-at')).toHaveText(['Review the packet', 'Review the packet', '']);
    await expect(panel.locator('td.waiting-for')).toHaveText(['2h 0m', '≥2h 0m', '']);
    expect(await settledReads(page, () => asked.filter((p) => p === '/api/jobs').length, 2)).toBe(2);
    expect(asked.filter((p) => p === '/api/jobs/queue-age').length).toBe(1);
    for (const [i, j] of EXECUTIVE_PACKETS.entries()) {
      const row = panel.locator('tbody tr').nth(i);
      await row.locator('td').first().getByRole('link').click();
      await expect(page).toHaveURL(new RegExp(`/ux/jobs/${j.id}$`));
      await page.goBack();
      await expect(panel.locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
      await row.locator('td').nth(3).getByRole('link').click();
      await expect(page).toHaveURL(/\/ux\/jobs\?subject_id=executive-subject$/);
      await page.goBack();
      await expect(panel.locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
      await row.focus();
      await row.press(i === 1 ? 'Space' : 'Enter');
      await expect(page).toHaveURL(new RegExp(`/ux/jobs/${j.id}$`));
      await page.goBack();
      await expect(panel.locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
      await row.locator('td').nth(2).click();
      await expect(page).toHaveURL(new RegExp(`/ux/jobs/${j.id}$`));
      await page.goBack();
      await expect(panel.locator('h3')).toHaveText(['In (1)', 'Working (1)', 'Out (1)']);
    }
  });

  test('every card control is keyboard reachable on a phone and Back retains the overview', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await install(page);
    await mountPage(page, PATH);
    for (const title of ['Waiting on you', 'Revenue mix', 'Cash & receivables', 'Active jobs', 'Executive work']) {
      const link = card(page, title).locator('.card-link a');
      const target = await link.getAttribute('href');
      expect(target).not.toBeNull();
      await link.focus();
      await expect(link).toBeFocused();
      await link.press('Enter');
      await expect(page).toHaveURL(new RegExp(`${target!.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}$`));
      await expect(page.locator('.exec')).toHaveCount(0);
      await page.goBack();
      await expect(page.locator('section.exec-card')).toHaveCount(5);
      await expect(page.locator('.exec-subtitle')).toHaveText('Money, open work, and what is waiting on you');
    }
  });
});
