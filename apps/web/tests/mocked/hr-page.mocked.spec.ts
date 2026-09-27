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
import { mountPage } from './_helpers';
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
