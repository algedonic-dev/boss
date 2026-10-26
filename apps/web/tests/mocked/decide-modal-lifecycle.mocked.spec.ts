// Decide-modal lifecycle (2026-08-21 UX audit: "one blip poisons it;
// a silent success strands the row").
//
// Two defects, one flow. (a) `error` was set in load()'s catch and
// never cleared, and the render puts the error before the step — so
// one transient failure replaced the working surface until
// close+reopen. (b) When the reload AFTER a successful save failed,
// `step` kept its pre-save status, the completed-check never fired,
// and onDecided never ran — a successfully decided row stayed in
// "Yours to decide", inviting a second decide to PUT completed over
// it. The fix: a failed post-save reload tells the page to refetch
// anyway and surfaces a soft note, never the poisoning error.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const JOB_ID = '99cfb52b-fca1-4e69-8798-f02575faf592';
const STEP_ID = 'ab8036d5-0326-46be-9585-90c4636d9116';

const step = (status: string) => ({
  id: STEP_ID, job_id: JOB_ID, kind: 'task', title: 'Decide the design',
  assignee_id: 'emp-david', status, sort_order: 0, blocked_by: [],
  completed_on: null, metadata: {}, notes: null, fields: [],
});

const row = () => ({
  job_id: JOB_ID, job_title: 'Feedback on /ux/jobs', due_on: null,
  opened_on: new Date(Date.now() - 20 * 86_400_000).toISOString().slice(0, 10),
  workflow: 'user-feedback', subject_kind: 'custom', subject_id: '/ux/jobs',
  priority: 'standard', simulated: false, tags: [],
  step: { ...step('active'), completion: 'human', decision_shaped: true },
});

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

/// The stateful world: an active decision step; completing it flips
/// the state; the job GET can be told to fail N times.
function world() {
  return { completed: false, jobGetFailures: 0, human: false, legacy: false };
}

async function mocks(page: Page, w: ReturnType<typeof world>) {
  // Catch-all FIRST: Playwright matches routes in reverse registration
  // order, so later, more specific routes win over this one.
  await page.route('**/api/**', (r) => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: 'emp-david', role: 'platform-admin' }));
  const metadata = () => w.human ? { human_only: true, ...(w.legacy ? {} : { human_presentation: {
    ask: 'Authorize this exact receiver', why_person: 'Your passkey authorizes this receiver only',
    action: 'Inspect and authorize', after: 'The authorized delivery can proceed', evidence: 'current-step',
  } }) } : {};
  await page.route(/\/api\/jobs\/assignments/, (r) =>
    json(r, { data: w.completed ? [] : [{ ...row(), step: { ...row().step, metadata: metadata() } }] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => {
    if (w.jobGetFailures > 0) {
      w.jobGetFailures -= 1;
      return json(r, 'boom', 500);
    }
    return json(r, {
      id: JOB_ID, kind: 'user-feedback', title: 'Feedback on /ux/jobs',
      status: 'open', subject: { subject_kind: 'custom', id: '/ux/jobs' },
      owner_id: 'emp-david', metadata: {},
      steps: [{ ...step(w.completed ? 'completed' : 'active'), metadata: metadata() }],
    });
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/${STEP_ID}$`), (r) => {
    if (r.request().method() === 'PUT') w.completed = true;
    return json(r, step(w.completed ? 'completed' : 'active'));
  });
}

test('a declared human ask keeps the packet and step titles in My Day and the modal', async ({ page }) => {
  const w = world(); w.human = true;
  await mocks(page, w);
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Authorize this exact receiver' })).toBeVisible();
  await expect(page.getByText('Your passkey authorizes this receiver only')).toBeVisible();
  await expect(page.getByText('Feedback on /ux/jobs', { exact: true })).toBeVisible();
  await expect(page.getByText('Decide the design', { exact: true })).toBeVisible();
  const verdictRow = page.locator('.myday-verdict-row');
  await expect(verdictRow.locator('.myday-age')).toHaveText(/open 20 d/i);
  await expect(verdictRow.locator('.myday-age')).toHaveAttribute('data-band', 'stale');
  await expect(verdictRow.locator('.myday-claim-btn')).toHaveText('Decide');
  await page.getByRole('button', { name: 'Inspect and authorize' }).click();
  const modal = page.locator('.dm');
  await expect(modal.getByRole('heading', { name: 'Decide the design', level: 2, exact: true })).toBeVisible();
  await expect(modal.getByText('Feedback on /ux/jobs', { exact: true })).toBeVisible();
  await expect(modal.getByRole('heading', { name: 'Authorize this exact receiver', exact: true })).toHaveCount(1);
  await expect(modal.locator('.step-generic')).toBeVisible();
  await expect(modal.getByRole('link', { name: 'Read the evidence and full instructions' })).toHaveAttribute('href', new RegExp(STEP_ID));
});

test('a human request without presentation keeps the legacy packet card and real modal titles', async ({ page }) => {
  const w = world(); w.human = true; w.legacy = true;
  await mocks(page, w);
  await page.goto('/');
  const card = page.locator('.myday-verdict-row .packet');
  await expect(card).toContainText('Feedback on /ux/jobs');
  await expect(card).toContainText('Decide the design');
  await expect(page.getByText('Instructions for you are unavailable', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Decide', exact: true }).click();
  const modal = page.locator('.dm');
  await expect(modal.getByRole('heading', { name: 'Decide the design', level: 2, exact: true })).toBeVisible();
  await expect(modal.getByText('Feedback on /ux/jobs', { exact: true })).toBeVisible();
  await expect(modal.locator('.human-ask')).toHaveCount(0);
  await expect(modal.locator('.step-generic')).toBeVisible();
});

test('a decide whose reload blips does not poison the modal, and the row still leaves', async ({ page }) => {
  const w = world();
  await mocks(page, w);
  await page.goto('/');

  // The verdict row is waiting, with its Decide affordance.
  await page.getByRole('button', { name: 'Decide' }).click();
  const modal = page.locator('.dm');
  await expect(modal).toBeVisible();
  await expect(modal.locator('.step-generic')).toBeVisible();

  // The save will succeed; the re-read right after it will not.
  w.jobGetFailures = 1;
  await modal.getByRole('button', { name: 'Complete' }).click();

  // The soft note, not the poisoning error: the surface reported a
  // successful save and is showing its receipt.
  await expect(modal.locator('.dm-note')).toContainText('Saved');
  await expect(modal.locator('.dm-error')).toHaveCount(0);

  // And onDecided ran anyway: the queue behind the modal refetched,
  // so the decided row has left "Yours to decide".
  await page.locator('.dm-head').getByRole('button', { name: 'Close' }).click();
  await expect(page.getByText('No verdicts waiting on you.')).toBeVisible();
});

test('a decide whose reload lands keeps the quiet path: no note, no error, row gone', async ({ page }) => {
  const w = world();
  await mocks(page, w);
  await page.goto('/');

  await page.getByRole('button', { name: 'Decide' }).click();
  const modal = page.locator('.dm');
  await expect(modal.locator('.step-generic')).toBeVisible();

  await modal.getByRole('button', { name: 'Complete' }).click();

  // The reload saw the completed step: no aside to make, nothing to
  // apologise for.
  await expect(modal.locator('.step-status-completed')).toBeVisible();
  await expect(modal.locator('.dm-note')).toHaveCount(0);
  await expect(modal.locator('.dm-error')).toHaveCount(0);

  await page.locator('.dm-head').getByRole('button', { name: 'Close' }).click();
  await expect(page.getByText('No verdicts waiting on you.')).toBeVisible();
});

test('a modal that cannot load at all still says so', async ({ page }) => {
  // The error state remains the truthful answer when there is nothing
  // to show INSTEAD of it — only its precedence over a working
  // surface was the bug.
  const w = world();
  await mocks(page, w);
  await page.goto('/');

  w.jobGetFailures = 99;
  await page.getByRole('button', { name: 'Decide' }).click();
  await expect(page.locator('.dm .dm-error')).toContainText('HTTP 500');
});
