import { isPageWrite } from './_smokeMocks';
// HR's task table is a read view, not a second step executor (36132827,
// audit b959394e). Required fields, sign-offs and plugins belong on the
// existing Job surface. No task transition may leave this table.
import { expect, test, type Page, type Route } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, servePeopleRows } from './_smokeMocks';
import { parseRoute } from '../../src/router';

const JOB = '12345678-1234-4234-8234-123456789abc';
const json = (r: Route, body: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
const person = {
  id: 'emp-owner', name: 'HR Owner', email: 'owner@example.test', role: 'platform-admin',
  department: 'people', hire_date: '2023-01-01', status: 'active', location: null,
  employment_type: 'full-time', skills: [], certifications: [],
};
const steps = ['ready', 'completed'].map((status, sort_order) => ({
  id: `step-${sort_order}`, job_id: JOB, kind: 'task', title: `HR task ${sort_order}`,
  status, sort_order, assignee_id: null, blocked_by: [], fields: [],
  sign_offs_required: [], sign_offs: [], metadata: {}, completed_on: null,
}));
const packet = {
  id: JOB, kind: 'hr-routing-fixture', workflow_version: 1, title: 'HR onboarding packet',
  status: 'open', priority: 'standard', owner_id: 'emp-owner',
  subject: { subject_kind: 'employee', id: 'emp-owner' },
  opened_on: '2026-01-01', due_on: null, closed_on: null, metadata: {}, steps,
};

async function openTasks(page: Page, stepsStatus = 200, jobStatus = 200): Promise<string[]> {
  const writes: string[] = [];
  page.on('request', (r) => {
    if (isPageWrite(r.method(), new URL(r.url()).pathname)) {
      writes.push(`${r.method()} ${r.url()}`);
    }
  });
  await installSmokeMocks(page);
  await servePeopleRows(page, [person]);
  await page.route(/\/api\/workflows$/, (r) => json(r, [{
    kind: packet.kind, label: 'HR onboarding', version: 1, status: 'active',
    subject_kinds: ['employee'], metadata: { surfaces: ['hr'] }, steps: [],
  }]));
  await page.route(/\/api\/jobs\?kind=hr-routing-fixture/, (r) => json(r, { data: [packet], total: 1 }));
  await page.route(new RegExp(`/api/jobs/${JOB}/steps$`), (r) =>
    json(r, stepsStatus === 200 ? steps : { error: 'policy refuses this read' }, stepsStatus));
  await page.route(new RegExp(`/api/jobs/${JOB}$`), (r) =>
    json(r, jobStatus === 200 ? packet : { error: 'policy refuses this packet' }, jobStatus));
  await mountPage(page, '/hr');
  await page.getByRole('tab', { name: 'Workflows', exact: true }).click();
  await page.getByRole('button', { name: 'View tasks', exact: true }).click();
  return writes;
}

test('every HR task opens its exact Job instead of offering Mark done; destination and Back are real', async ({ page }) => {
  const writes = await openTasks(page);
  const links = page.getByRole('link', { name: 'Open job', exact: true });
  await expect(links).toHaveCount(2);
  await expect(page.getByRole('button', { name: 'Mark done', exact: true })).toHaveCount(0);
  expect(parseRoute(`/jobs/${JOB}`)).toEqual({ kind: 'jobDetail', jobId: JOB });
  for (let i = 0; i < 2; i += 1) {
    await expect(links.nth(i)).toHaveAttribute('href', `/jobs/${JOB}`);
  }
  await links.first().click();
  await expect(page).toHaveURL(new RegExp(`/jobs/${JOB}$`));
  await expect(page.locator('h1').first()).toContainText(packet.title);
  await page.goBack();
  await expect(page).toHaveURL(/\/hr$/);
  await expect(page.getByRole('tab', { name: 'Workflows', exact: true })).toBeVisible();
  expect(writes).toEqual([]);
});

test('a refused task read stays a failure and offers no invented task action', async ({ page }) => {
  const writes = await openTasks(page, 403);
  await expect(page.locator('.load-failed').filter({ hasText: 'onboarding steps' })).toContainText('HTTP 403');
  await expect(page.getByRole('link', { name: 'Open job', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Mark done', exact: true })).toHaveCount(0);
  expect(writes).toEqual([]);
});

test('a refused destination packet remains refused on the existing Job surface', async ({ page }) => {
  const writes = await openTasks(page, 200, 403);
  await page.getByRole('link', { name: 'Open job', exact: true }).first().click();
  await expect(page).toHaveURL(new RegExp(`/jobs/${JOB}$`));
  await expect(page.locator('.load-failed')).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/\/hr$/);
  expect(writes).toEqual([]);
});
