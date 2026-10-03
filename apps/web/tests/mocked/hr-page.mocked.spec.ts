// /hr, page audit b959394e (2026-09-27). One spec for the page's own
// decisions, each pinned where it renders:
//
//   d8d48a49 — the header counted the `[]` the roster starts as, so while
//              the read was in flight, and above "Couldn't load the
//              roster" when it failed, /hr said "0 active employees". It
//              now builds its header through people/roster-counts.ts
//              rosterHeader(), the helper /ux/people already used, so the
//              two People pages say "unknown" in the same words.
//   ad2da739 — every count on the page is taken over the roster
//              headcount() counts: emp-audit, the System Audit Account,
//              was in the title, At a glance and the Headcount table.
//   0ab0fbac — the Requisitions tab and five literal zeros (subtitle, At
//              a glance, the Open reqs and Target columns, the total row)
//              stated what no read had said. Gone.
//   e2f7cbb8 — the Workflows tab said nothing when the roster read had
//              failed, so its empty employee picker read as a company with
//              nobody in it.
//   5b27ed56 — Active Workflows rows were keyed and opened by EMPLOYEE,
//              so one person with two open HR Jobs had a second row whose
//              "View tasks" showed the first Job's steps.
//   603a4185 — "Start {label}" set window.location.href: a full reload of
//              the shell, bypassing the router.
//
// The two workflow reads' failure lines (R2 /api/workflows, R3 the open
// Jobs of each HR kind) are pinned in false-empty.mocked.spec.ts beside
// the step read's; the roster read's line is asserted by the outage
// crawl, which breaks /api/people on /hr (c69e7455).

import { expect, test, type Page, type Route } from './_test';
import { mountPage, openedRequests, recordPageRequests } from './_helpers';
import { installSmokeMocks, servePeopleRows } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

function emp(
  id: string,
  name: string,
  role: string,
  employment_type = 'full-time',
): Readonly<{ id: string } & Record<string, unknown>> {
  return {
    id, name, email: `${id}@a`, role, department: 'it', hire_date: '2023-01-01',
    status: 'active', location: 'HQ', employment_type, skills: [], certifications: [],
  };
}

const OWNER = emp('emp-david', 'Dee Owner', 'platform-admin');
/// The System Audit Account the operator baseline ships in every
/// deployment, as the live roster has it: the only contractor.
const AUDIT = emp('emp-audit', 'System Audit Account', 'audit-readonly', 'contractor');

const klass = (member_attribute: string, code: string, metadata: Record<string, unknown>) => ({
  subject_kind: 'employee', code, display_name: code, parent_code: null,
  member_attribute, metadata, sort_order: 1, retired_at: null,
});

/// audit-readonly carries counts_in_headcount: false, as migration
/// 20260926223752 seeds it; platform-admin is a system role worn by a
/// person, and is counted.
const CLASSES = [
  klass('role', 'platform-admin', { is_system_role: true }),
  klass('role', 'audit-readonly', { is_system_role: true, counts_in_headcount: false }),
  klass('department', 'it', {}),
];

const HR_KINDS = [
  { kind: 'onboarding', label: 'Onboarding' },
  { kind: 'offboarding', label: 'Offboarding' },
].map(({ kind, label }) => ({
  kind, label, version: 1, status: 'active', subject_kinds: ['employee'],
  metadata: { surfaces: ['hr'] }, metadata_schema: {}, entitlements: {},
  owning_team: 'hr', authoring_job_id: null, created_at: '2026-01-01T00:00:00Z', steps: [],
}));

const job = (id: string, kind: string, employeeId: string) => ({
  id, kind, status: 'open', subject: { subject_kind: 'employee', id: employeeId },
  owner_id: employeeId, title: `${kind} ${employeeId}`, metadata: {},
});

const step = (id: string, title: string) => ({
  id, kind: 'it-setup', title, status: 'ready', assignee_id: null, completed_on: null, metadata: {},
});

async function openHr(page: Page, people: (r: Route) => Promise<void>): Promise<void> {
  await installSmokeMocks(page);
  await servePeopleRows(page, [OWNER, AUDIT]);
  await page.route(/\/api\/people$/, people);
  await page.route(/\/api\/classes(\?|$)/, (r) => json(r, CLASSES));
  await page.route(/\/api\/workflows$/, (r) => json(r, HR_KINDS));
  // Dee Owner has TWO open HR Jobs at once: a hire and a leave.
  await page.route(/\/api\/jobs\?kind=onboarding/, (r) =>
    json(r, { data: [job('job-hire', 'onboarding', 'emp-david')], total: 1 }));
  await page.route(/\/api\/jobs\?kind=offboarding/, (r) =>
    json(r, { data: [job('job-leave', 'offboarding', 'emp-david')], total: 1 }));
  await page.route(/\/api\/jobs\/job-hire\/steps$/, (r) => json(r, [step('s-hire', 'Issue a laptop')]));
  await page.route(/\/api\/jobs\/job-leave\/steps$/, (r) => json(r, [step('s-leave', 'Return the laptop')]));
  await mountPage(page, '/hr');
}

const title = (page: Page) => page.locator('.exec-title');
const header = (page: Page) => page.locator('header.exec-header');
const tab = (page: Page, name: string) => page.getByRole('tab', { name, exact: true });

test.describe('/hr — page audit b959394e', () => {
  for (const site of ['urgent', 'workflow', 'certifications'] as const) {
    test(`${site} employee link opens the catalogued employee surface and Back returns to HR`, async ({ page }) => {
      await page.clock.install({ time: new Date('2026-10-03T12:00:00Z') });
      const certified = {
        ...OWNER,
        certifications: [{ name: 'Safety training', issuing_body: 'Training Authority', expires_on: '2026-10-15' }],
      };
      await openHr(page, (r) => json(r, [certified]));
      await page.route(/\/api\/people\/emp-david$/, (r) => json(r, certified));
      if (site === 'workflow') await tab(page, 'Workflows').click();
      if (site === 'certifications') await tab(page, 'Certifications').click();
      const link = page.getByRole('link', { name: 'Dee Owner', exact: true }).first();
      await expect(link).toHaveAttribute('href', '/ux/people/emp-david');
      await link.click();
      await expect(page).toHaveURL(/\/ux\/people\/emp-david$/);
      await expect(page.getByRole('heading', { name: 'Dee Owner', exact: true })).toBeVisible();
      await page.goBack();
      await expect(page).toHaveURL(/\/hr$/);
      await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
      await expect(page.getByRole('link', { name: 'Dee Owner', exact: true })).toBeVisible();
    });
  }

  test('all four tabs select their own panel and returning to Overview restores its data', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));
    for (const [name, heading] of [
      ['Workflows', 'Start Workflow'],
      ['Certifications', 'Expiring in 90 days (0)'],
      ['Headcount', 'Headcount by department'],
      ['Overview', 'At a glance'],
    ] as const) {
      await tab(page, name).click();
      await expect(tab(page, name)).toHaveAttribute('aria-selected', 'true');
      await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible();
      await expect(page.getByRole('tab', { selected: true })).toHaveCount(1);
    }
    await expect(page.getByText('Nothing urgent today.')).toBeVisible();
  });

  test('urgent shows at most five links while Certifications shows the complete ninety-day population', async ({ page }) => {
    await page.clock.install({ time: new Date('2026-10-03T12:00:00Z') });
    const rows = Array.from({ length: 6 }, (_, n) => ({
      ...emp(`emp-${n}`, `Employee ${n}`, 'platform-admin'),
      certifications: [{ name: `Training ${n}`, issuing_body: 'Training Authority', expires_on: '2026-10-15' }],
    }));
    const later = {
      ...emp('emp-later', 'Later Employee', 'platform-admin'),
      certifications: [{ name: 'Later training', issuing_body: 'Later Authority', expires_on: '2026-12-01' }],
    };
    await openHr(page, (r) => json(r, [...rows, later]));
    await expect(header(page)).toContainText('7 certifications expiring in 90 days');
    await expect(page.getByRole('heading', { name: '6 certs expiring in 30 days', exact: true })).toBeVisible();
    await expect(page.locator('.tab-panel').getByRole('link')).toHaveCount(5);
    await tab(page, 'Certifications').click();
    await expect(page.getByRole('heading', { name: 'Expiring in 90 days (7)', exact: true })).toBeVisible();
    await expect(page.locator('table tbody tr')).toHaveCount(7);
    await expect(page.locator('tr').filter({ hasText: 'Later Employee' })).toContainText('Later Authority');
    await expect(page.locator('tr').filter({ hasText: 'Later Employee' })).toContainText('2026-12-01');
  });

  test('empty successful reads keep the roster, workflow and certification empty states distinct', async ({ page }) => {
    await openHr(page, (r) => json(r, []));
    await page.route(/\/api\/workflows$/, (r) => json(r, []));
    await expect(title(page)).toHaveText('0 active employees');
    await expect(page.getByText('Nothing urgent today.')).toBeVisible();
    await tab(page, 'Workflows').click();
    await expect(page.getByText('No HR workflows are published in this deployment.', { exact: false })).toBeVisible();
    await expect(page.getByText('No active workflows.', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: /^Start / })).toHaveCount(0);
    await tab(page, 'Certifications').click();
    await expect(page.getByText('No certifications expiring in the next 90 days.')).toBeVisible();
    await expect(page.locator('.load-failed')).toHaveCount(0);
  });

  test('Start buttons require a selection and the picker admits active and on-leave employees only', async ({ page }) => {
    const leave = { ...emp('emp-leave', 'Lee Leave', 'platform-admin'), status: 'on-leave' };
    const former = { ...emp('emp-former', 'Former Employee', 'platform-admin'), status: 'terminated' };
    await openHr(page, (r) => json(r, [OWNER, leave, former]));
    await tab(page, 'Workflows').click();
    await expect(page.getByRole('button', { name: 'Start Onboarding' })).toBeDisabled();
    await expect(page.getByRole('button', { name: 'Start Offboarding' })).toBeDisabled();
    await expect(page.locator('select.hr-select option')).toHaveText([
      'Select employee...', 'Dee Owner (emp-david)', 'Lee Leave (emp-leave)',
    ]);
    await page.locator('select.hr-select').selectOption('emp-leave');
    await expect(page.getByRole('button', { name: 'Start Offboarding' })).toBeEnabled();
    await page.getByRole('button', { name: 'Start Offboarding' }).click();
    await expect(page).toHaveURL(/\/jobs\?new=1&kind=offboarding&subject_kind=employee&subject_id=emp-leave$/);
    await page.goBack();
    await expect(title(page)).toHaveText('1 active employee');
    await expect(tab(page, 'Overview')).toHaveAttribute('aria-selected', 'true');
  });

  for (const name of ['Overview', 'Certifications', 'Headcount']) {
    test(`${name} renders the roster refusal instead of derived empty data`, async ({ page }) => {
      await openHr(page, (r) => json(r, 'roster unavailable', 503));
      await tab(page, name).click();
      await expect(page.locator('.load-failed')).toContainText("Couldn't load the roster — HTTP 503");
      await expect(page.getByText('Nothing urgent today.')).toHaveCount(0);
      await expect(page.getByText('No certifications expiring in the next 90 days.')).toHaveCount(0);
      await expect(page.locator('table.data-table')).toHaveCount(0);
    });
  }

  test('registry refusal explains both unavailable workflow discovery and the active list', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));
    await page.route(/\/api\/workflows$/, (r) => json(r, 'registry unavailable', 503));
    await tab(page, 'Workflows').click();
    await expect(page.locator('.load-failed')).toHaveText([
      "Couldn't load HR workflows — workflows registry: HTTP 503",
      "Couldn't load active workflows — workflows registry: HTTP 503",
    ]);
    await expect(page.getByText('No active workflows.', { exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: /^Start / })).toHaveCount(0);
  });

  test('failed role classes declare the fallback instead of claiming excluded-account headcount', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER, AUDIT]));
    // Register before remounting: the shared session loads classes once
    // per document, so changing a response after it was read is no test.
    await page.route(/\/api\/classes(\?|$)/, (r) => json(r, 'classes unavailable', 503));
    await page.reload();
    await expect(page.locator('.catalog.theme-exec .load-failed').filter({ hasText: "Couldn't load the roles" }))
      .toContainText('Roles show by code, and the counts include every role.');
    await expect(title(page)).toHaveText('2 active employees');
  });

  test('a failed kind list is unknown even when another kind was read successfully', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));
    await page.route(/\/api\/jobs\?kind=offboarding/, (r) => json(r, 'jobs unavailable', 503));
    await tab(page, 'Workflows').click();
    await expect(page.locator('.load-failed')).toContainText("Couldn't load active workflows — offboarding jobs:");
    await expect(page.locator('.load-failed')).toContainText('503');
    await expect(page.getByText('No active workflows.', { exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'View tasks' })).toHaveCount(0);
  });

  test('workflow discovery follows registry surfaces and employee subjects rather than fixed kind names', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));
    const custom = { ...HR_KINDS[0], kind: 'employee-review', label: 'Employee Review' };
    await page.route(/\/api\/workflows$/, (r) => json(r, [
      custom,
      { ...custom, kind: 'asset-review', label: 'Asset Review', subject_kinds: ['asset'] },
      { ...custom, kind: 'unlisted-review', label: 'Unlisted Review', metadata: { surfaces: [] } },
    ]));
    await page.route(/\/api\/jobs\?kind=employee-review/, (r) => json(r, { data: [], total: 0 }));
    await tab(page, 'Workflows').click();
    await expect(page.getByRole('button', { name: /^Start / })).toHaveText(['Start Employee Review']);
    await expect(page.getByText('No active workflows.', { exact: true })).toBeVisible();
    await page.locator('select.hr-select').selectOption('emp-david');
    await page.getByRole('button', { name: 'Start Employee Review' }).click();
    await expect(page).toHaveURL(/kind=employee-review&subject_kind=employee&subject_id=emp-david$/);
  });

  test('an unrecognised jobs envelope renders a failed list rather than an empty department', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));
    await page.route(/\/api\/jobs\?kind=onboarding/, (r) => json(r, { jobs: [], total: 0 }));
    await tab(page, 'Workflows').click();
    await expect(page.locator('.load-failed')).toContainText("Couldn't load active workflows — onboarding jobs:");
    await expect(page.getByText('No active workflows.', { exact: true })).toHaveCount(0);
  });

  for (const answer of ['failed', 'malformed', 'empty'] as const) {
    test(`View tasks keeps a ${answer} step read distinct`, async ({ page }) => {
      await openHr(page, (r) => json(r, [OWNER]));
      await page.route(/\/api\/jobs\/job-hire\/steps$/, (r) =>
        answer === 'failed' ? json(r, 'steps unavailable', 503)
          : answer === 'malformed' ? json(r, { unexpected: [] }) : json(r, []));
      await tab(page, 'Workflows').click();
      if (answer !== 'empty') {
        await expect(page.locator('tr').filter({ hasText: 'Onboarding' })).toContainText('Progress unknown');
      }
      await page.getByRole('button', { name: 'View tasks' }).first().click();
      if (answer === 'empty') {
        await expect(page.getByText('This workflow has no steps — the Job was read and it is genuinely empty.', { exact: false })).toBeVisible();
        await expect(page.locator('.load-failed')).toHaveCount(0);
      } else {
        await expect(page.getByRole('alert')).toContainText("Couldn't read Dee Owner's onboarding steps");
        await expect(page.getByRole('alert')).toContainText('unknown, not absent');
        await expect(page.getByText('This workflow has no steps', { exact: false })).toHaveCount(0);
      }
      await expect(page.getByRole('button', { name: 'Mark done' })).toHaveCount(0);
    });
  }

  test('task rows open their own Job for completion evidence without a task-table write (36132827)', async ({ page }) => {
    await recordPageRequests(page);
    await openHr(page, (r) => json(r, [OWNER]));
    await page.route(/\/api\/jobs\/job-hire\/steps$/, (r) => json(r, [
      step('s-hire', 'Issue a laptop'),
      { ...step('s-accepted', 'Record acceptance'), status: 'completed', completed_on: '2026-10-02' },
    ]));
    await page.route(/\/api\/jobs\/job-hire$/, (r) => json(r, {
      ...job('job-hire', 'onboarding', 'emp-david'), steps: [],
    }));
    await tab(page, 'Workflows').click();
    await page.getByRole('button', { name: 'View tasks' }).first().click();
    for (const title of ['Issue a laptop', 'Record acceptance']) {
      await expect(page.locator('tr').filter({ hasText: title }).getByRole('link', { name: 'Open job' }))
        .toHaveAttribute('href', '/jobs/job-hire');
    }
    await expect(page.getByRole('button', { name: 'Mark done' })).toHaveCount(0);
    await page.locator('tr').filter({ hasText: 'Issue a laptop' }).getByRole('link', { name: 'Open job' }).click();
    await expect(page).toHaveURL(/\/jobs\/job-hire$/);
    await expect(page.getByRole('heading', { name: 'onboarding emp-david', exact: true })).toBeVisible();
    expect((await openedRequests(page)).filter((r) => r.method !== 'GET' && r.path !== '/api/surface-opens'))
      .toEqual([]);
    await page.goBack();
    await expect(page).toHaveURL(/\/hr$/);
    await expect(title(page)).toHaveText('1 active employee');
  });

  test('a failed roster read states no count in the header (d8d48a49)', async ({ page }) => {
    await openHr(page, (r) => json(r, 'people store down', 500));

    await expect(page.locator('.load-failed')).toContainText("Couldn't load the roster");
    await expect(title(page)).toHaveText('Active employees');
    await expect(header(page)).toContainText('Counts unknown: the roster did not load');
    await expect(header(page)).not.toContainText(/\d/);
  });

  test('a roster still loading states no count, then counts once it is read (d8d48a49)', async ({ page }) => {
    let release: () => void = () => {};
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    await openHr(page, async (r) => {
      await held;
      await json(r, [OWNER, AUDIT]);
    });

    await expect(page.getByText('Loading…').first()).toBeVisible();
    await expect(title(page)).toHaveText('Active employees');
    await expect(header(page)).toContainText('Loading the roster…');

    release();

    await expect(title(page)).toHaveText('1 active employee');
    await expect(header(page)).toContainText('0 certifications expiring in 90 days');
  });

  test('the System Audit Account is in no count on the page (ad2da739)', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER, AUDIT]));

    await expect(title(page)).toHaveText('1 active employee');
    const glance = page.locator('dl.kv');
    const value = (label: string) =>
      glance.locator('dt').filter({ hasText: new RegExp(`^${label}$`) }).locator('xpath=following-sibling::dd[1]');
    await expect(value('Total headcount')).toHaveText('1');
    await expect(value('Active')).toHaveText('1');
    await expect(value('Contractors')).toHaveText('0');

    await tab(page, 'Headcount').click();
    const rows = page.locator('table.data-table tbody tr');
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0).locator('td').nth(1)).toHaveText('1');
    await expect(rows.last()).toContainText('Total');
    await expect(rows.last().locator('td').nth(1)).toHaveText('1');
  });

  test('no requisition is stated: no tab, no literal zeros (0ab0fbac)', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));

    await expect(title(page)).toHaveText('1 active employee');
    await expect(tab(page, 'Requisitions')).toHaveCount(0);
    await expect(page.getByRole('tab')).toHaveText(['Overview', 'Workflows', 'Certifications', 'Headcount']);
    const hr = page.locator('.catalog.theme-exec');
    await expect(hr.getByText('Total headcount')).toBeVisible();
    await expect(hr).not.toContainText(/requisition|open reqs/i);

    await tab(page, 'Headcount').click();
    await expect(page.locator('table.data-table thead th')).toHaveText(['Department', 'Active', 'On leave']);
  });

  test('the Workflows tab says the roster did not load above its empty picker (e2f7cbb8)', async ({ page }) => {
    await openHr(page, (r) => json(r, 'people store down', 500));
    await tab(page, 'Workflows').click();

    const failed = page.locator('.load-failed').filter({ hasText: "Couldn't load the roster" });
    await expect(failed).toBeVisible();
    await expect(failed).toContainText('HTTP 500');
    // It stands above the picker, which it explains.
    const picker = page.locator('select.hr-select');
    await expect(picker).toBeVisible();
    const failedBox = await failed.boundingBox();
    const pickerBox = await picker.boundingBox();
    expect(failedBox && pickerBox && failedBox.y < pickerBox.y).toBe(true);
  });

  test("two open HR Jobs for one person are two rows, each opening its own Job's tasks (5b27ed56)", async ({ page }) => {
    const errors: string[] = [];
    page.on('console', (m) => {
      if (m.type() === 'error' || m.type() === 'warning') errors.push(m.text());
    });
    await openHr(page, (r) => json(r, [OWNER, AUDIT]));
    await tab(page, 'Workflows').click();

    const rows = page.locator('table.data-table tbody tr');
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0)).toContainText('Onboarding');
    await expect(rows.nth(1)).toContainText('Offboarding');

    await rows.nth(1).getByRole('button', { name: 'View tasks' }).click();
    await expect(page.getByText('Return the laptop')).toBeVisible();
    await expect(page.getByText('Issue a laptop')).toHaveCount(0);

    await rows.nth(0).getByRole('button', { name: 'View tasks' }).click();
    await expect(page.getByText('Issue a laptop')).toBeVisible();
    await expect(page.getByText('Return the laptop')).toHaveCount(0);

    expect(errors.filter((e) => e.includes('each_key_duplicate'))).toEqual([]);
  });

  test('Start {label} lands on the new-Job form through the router, not a reload (603a4185)', async ({ page }) => {
    await openHr(page, (r) => json(r, [OWNER]));
    await tab(page, 'Workflows').click();

    await page.locator('select.hr-select').selectOption('emp-david');
    // A marker on the window survives a pushState and dies with a reload.
    await page.evaluate(() => {
      (window as unknown as { hrSameDocument?: boolean }).hrSameDocument = true;
    });
    await page.getByRole('button', { name: 'Start Onboarding' }).click();

    await expect(page).toHaveURL(
      /\/jobs\?new=1&kind=onboarding&subject_kind=employee&subject_id=emp-david$/,
    );
    expect(
      await page.evaluate(() => (window as unknown as { hrSameDocument?: boolean }).hrSameDocument),
    ).toBe(true);
  });
});
