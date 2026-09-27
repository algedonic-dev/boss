// Design 611fbffd ("A step becomes Active only through a claim", answered
// by David 2026-09-26), clause (b) of backlog 6ef4a36b item (2). Every
// step surface's Start was its own `PUT {status: 'active'}` naming the
// holder, so the record could not tell "X took this work" from "someone
// assigned X and started the clock", and "release, then claim" held only
// as far as every writer chose to honour it. A Start now goes through
// the claim door: the page saves what it holds, then claims — for the
// holder it shows, else for the operator — and no write it makes carries
// a status of `active`.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-start-1';

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};
const BREWER = { ...OPERATOR, id: 'emp-brewer', name: 'Brewer', role: 'brewer' };

const step = (status: string, assignee: string | null) => ({
  id: 's1', job_id: JOB_ID, kind: 'task', title: 'mash in', status,
  assignee_id: assignee, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], completed_on: null, metadata: {}, notes: null, spec_slug: 'mash',
});

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Write = { method: string; path: string; query: string; body: unknown };

/// Mocks the page around one step and records every write to it — the
/// step PUT, the merge door and the claim door alike. `claimStatus`
/// answers the claim.
async function mocks(
  page: Page,
  s: ReturnType<typeof step>,
  claimStatus = 200,
): Promise<Write[]> {
  const writes: Write[] = [];
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, {
    id: JOB_ID, kind: 'brew-day', title: 'Brew day',
    status: 'open', opened_on: '2026-09-26', due_on: null, closed_on: null,
    owner_id: 'emp-001', priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'batch-1' }, metadata: {}, steps: [s],
  }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR, BREWER]));
  await servePeopleRows(page, [OPERATOR, BREWER]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1(/metadata|/claim)?(\\?.*)?$`), (r) => {
    const req = r.request();
    const url = new URL(req.url());
    writes.push({
      method: req.method(),
      path: url.pathname,
      query: url.search,
      body: req.postData() ? req.postDataJSON() : undefined,
    });
    if (url.pathname.endsWith('/claim')) {
      return claimStatus === 200
        ? json(r, { ...s, status: 'active', assignee_id: s.assignee_id ?? OPERATOR.id })
        : json(r, { error: 'step already claimed or not claimable', holder: 'agent-claude', status: 'active' }, claimStatus);
    }
    return r.fulfill({ status: 204, body: '' });
  });
  return writes;
}

const carriesActive = (w: Write): boolean =>
  !!w.body && typeof w.body === 'object' && (w.body as Record<string, unknown>)['status'] === 'active';

test('Start on an unassigned ready step claims it for the operator, through the claim door', async ({ page }) => {
  const writes = await mocks(page, step('ready', null));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await surface.getByRole('button', { name: 'Start' }).click();

  await expect.poll(() => writes.some((w) => w.path.endsWith('/claim'))).toBe(true);
  const claim = writes.find((w) => w.path.endsWith('/claim'))!;
  expect(claim).toEqual({
    method: 'POST',
    path: `/api/jobs/${JOB_ID}/steps/s1/claim`,
    query: '',
    body: undefined,
  });
  expect(writes.at(-1), 'the claim is the last write — the save lands first').toBe(claim);
  expect(writes.filter(carriesActive), 'no write moves the status by hand').toEqual([]);
});

test('Start on a nominated step claims it FOR the nominee the page shows', async ({ page }) => {
  const writes = await mocks(page, step('ready', BREWER.id));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await surface.getByRole('button', { name: 'Start' }).click();

  await expect.poll(() => writes.some((w) => w.path.endsWith('/claim'))).toBe(true);
  const claim = writes.find((w) => w.path.endsWith('/claim'))!;
  expect(claim.method).toBe('POST');
  expect(claim.query).toBe(`?claimed_for=${BREWER.id}`);
  expect(writes.filter(carriesActive)).toEqual([]);
});

test('a refused claim is shown in the server\'s words, not swallowed', async ({ page }) => {
  await mocks(page, step('ready', null), 409);
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await surface.getByRole('button', { name: 'Start' }).click();

  await expect(surface.getByRole('alert')).toContainText('step already claimed or not claimable');
});

test('a pending step offers no Start — its protocol opens it, not a hand on the page', async ({ page }) => {
  const writes = await mocks(page, step('pending', null));
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const surface = page.locator('.step-generic');
  await expect(surface).toBeVisible();
  await expect(surface.getByText('mash in')).toBeVisible();
  await expect(surface.getByRole('button', { name: 'Start' })).toHaveCount(0);
  expect(writes).toEqual([]);
});
