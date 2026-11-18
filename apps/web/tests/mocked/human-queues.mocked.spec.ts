import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const EMP = { id: 'emp-david', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };
const JOB = '19cfb52b-fca1-4e69-8798-f02575faf592';
const STEP = '1b8036d5-0326-46be-9585-90c4636d9116';
const presentation = { ask: 'Inspect this receiver', why_person: 'Your authorization is needed',
  action: 'Inspect and authorize', after: 'Delivery can proceed', evidence: 'current-step' };

async function mocks(page: Page, presented: boolean, assigned: boolean, refuse = false) {
  const seen = { claims: 0, reads: 0, assigned };
  const json = (r: Route, b: unknown, status = 200) =>
    r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });
  const metadata = { human_only: true, ...(presented ? { human_presentation: presentation } : {}) };
  const step = () => ({ id: STEP, job_id: JOB, kind: 'task', title: 'Authorize receiver',
    status: 'ready', assignee_id: seen.assigned ? EMP.id : null, completion: 'human',
    decision_shaped: false, agent_takes: false, metadata, sort_order: 0, blocked_by: [], fields: [] });
  await page.route('**/api/**', r => json(r, { data: [], total: 0 }));
  await page.route(/\/api\/people$/, r => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, r => json(r, {
    username: 'david', employee_id: EMP.id, role: EMP.role,
  }));
  await page.route(/\/api\/jobs\/assignments/, r => {
    seen.reads += 1;
    return json(r, { data: [{ job_id: JOB, job_title: 'Receiver delivery', due_on: null,
      workflow: 'user-feedback', subject_kind: 'custom', subject_id: 'receiver',
      priority: 'standard', simulated: false, tags: [], step: step() }] });
  });
  await page.route(new RegExp(`/api/jobs/${JOB}$`), r => json(r, {
    id: JOB, kind: 'user-feedback', title: 'Receiver delivery', status: 'open',
    subject: { subject_kind: 'custom', id: 'receiver' }, owner_id: EMP.id,
    metadata: {}, steps: [step()],
  }));
  await page.route(new RegExp(`/api/jobs/${JOB}/steps/${STEP}/claim$`), r => {
    expect(r.request().method()).toBe('POST');
    seen.claims += 1;
    if (refuse) return json(r, { error: 'already claimed by emp-other' }, 409);
    seen.assigned = true;
    return json(r, step());
  });
  return seen;
}

for (const presented of [true, false]) {
  test(`My queue keeps ${presented ? 'presented' : 'legacy'} work and its real modal`, async ({ page }) => {
    await mocks(page, presented, true);
    await page.goto('/');
    const card = page.locator(presented ? '.human-ask' : '.packet').filter({ hasText: 'Receiver delivery' });
    await expect(card).toContainText('Authorize receiver');
    await expect(page.getByText('Instructions for you are unavailable', { exact: true })).toHaveCount(0);
    if (presented) {
      await expect(card).toContainText(presentation.why_person);
      await expect(card.getByRole('link')).toHaveAttribute('href', new RegExp(STEP));
      await card.getByRole('button', { name: presentation.action }).click();
      await expect(page.locator('.dm .step-generic')).toBeVisible();
      await expect(page.locator('.dm')).toContainText('Receiver delivery');
      await expect(page.locator('.dm')).toContainText('Authorize receiver');
    } else {
      await expect(card.locator('.human-ask')).toHaveCount(0);
    }
  });

  test(`Up for grabs claims and refetches ${presented ? 'presented' : 'legacy'} work`, async ({ page }) => {
    const seen = await mocks(page, presented, false);
    await page.goto('/');
    const grab = page.locator('.myday-grab-row').filter({ hasText: 'Receiver delivery' });
    await expect(grab).toContainText('Authorize receiver');
    if (presented) {
      await expect(grab).toContainText(presentation.ask);
      await expect(grab).toContainText(presentation.action);
      await expect(grab.getByRole('link')).toHaveAttribute('href', new RegExp(STEP));
    } else await expect(grab.locator('.packet')).toBeVisible();
    const reads = seen.reads;
    await grab.getByRole('button', { name: 'Claim', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Claim', exact: true })).toHaveCount(0);
    await expect(page.getByText('Receiver delivery', { exact: true })).toBeVisible();
    expect(seen.claims).toBe(1);
    expect(seen.reads).toBeGreaterThan(reads);
  });
}

test('a refused human claim retains the readable request and names the refusal', async ({ page }) => {
  const seen = await mocks(page, true, false, true);
  await page.goto('/');
  await page.getByRole('button', { name: 'Claim', exact: true }).click();
  await expect(page.locator('.myday-claim-note')).toHaveText('"Authorize receiver" was taken by someone else first');
  await expect(page.getByText('Receiver delivery', { exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: presentation.ask })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Claim', exact: true })).toBeVisible();
  expect(seen.claims).toBe(1);
});
