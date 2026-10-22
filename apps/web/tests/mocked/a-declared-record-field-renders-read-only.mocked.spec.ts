// Backlog 7ac7cd73. The generic step surface drew every declared field
// as a text input seeded only from a string, so a declared object field
// (ship-a-change's `release` record) showed as an EMPTY box over the
// record that was there, and a field a door writes (`hold_sha`) was
// offered for hand editing. A Workflow row could not declare what its
// doors write without the page lying about it. Now a declared record
// renders read-only as its record, a door-written field is not an input,
// and neither is ever sent back by Complete.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-record-1';

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const RELEASE = {
  sha: 'abc1234', review: 'rev-9', verdict: 'release', by: 'emp-001',
  at: '2026-10-19T12:00:00Z', main_sha: 'def5678',
};

const step = {
  id: 's1', job_id: JOB_ID, kind: 'task', title: 'review the car', status: 'active',
  assignee_id: OPERATOR.id, sort_order: 0, blocked_by: [], sign_offs_required: [],
  sign_offs: [], completed_on: null, notes: null, spec_slug: 'review',
  metadata: { release: RELEASE, hold_sha: 'abc1234' },
  fields: [
    { name: 'release', field_type: 'object' },
    { name: 'hold_sha', field_type: 'string', writer: 'runner:conductor' },
    { name: 'verdict_note', field_type: 'string', required: true },
  ],
};

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Write = { method: string; path: string; body: unknown };

async function mocks(page: Page): Promise<Write[]> {
  const writes: Write[] = [];
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, {
    id: JOB_ID, kind: 'ship-a-change', title: 'A car',
    status: 'open', opened_on: '2026-10-19', due_on: null, closed_on: null,
    owner_id: OPERATOR.id, priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'car-1' }, metadata: {}, steps: [step],
  }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR]));
  await servePeopleRows(page, [OPERATOR]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1(/metadata)?(\\?.*)?$`), (r) => {
    const req = r.request();
    writes.push({
      method: req.method(),
      path: new URL(req.url()).pathname,
      body: req.postData() ? req.postDataJSON() : undefined,
    });
    return r.fulfill({ status: 204, body: '' });
  });
  return writes;
}

test('a declared object field shows its record read-only and Complete never sends it back', async ({ page }) => {
  const writes = await mocks(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);

  const ask = page.locator('.step-generic .step-ask');
  await expect(ask).toBeVisible();

  // The record is on screen, whole, and is not an input.
  const record = ask.locator('[data-field="release"] .step-field-record');
  await expect(record).toContainText('"main_sha": "def5678"');
  await expect(record).toContainText('"verdict": "release"');
  await expect(ask.locator('[data-field="release"] input, [data-field="release"] textarea')).toHaveCount(0);

  // The door's field is shown with its writer, and offers no box.
  const held = ask.locator('[data-field="hold_sha"]');
  await expect(held.locator('.step-field-record')).toHaveText('abc1234');
  await expect(held).toContainText('runner:conductor');
  await expect(held.locator('input, textarea')).toHaveCount(0);

  // The executor's own answer is still a box.
  await ask.locator('[data-field="verdict_note"] textarea').fill('looks right');
  await ask.getByRole('button', { name: 'Complete' }).click();

  await expect.poll(() => writes.length).toBeGreaterThan(0);
  const sent = writes.map((w) => (w.body ?? {}) as Record<string, unknown>);
  const keys = sent.flatMap((b) => Object.keys((b['metadata'] ?? {}) as Record<string, unknown>)
    .concat(Object.keys(b).filter((k) => k !== 'metadata')));
  expect(keys).toContain('verdict_note');
  expect(keys, 'a record or a door-written key is never written from the page').not.toContain('release');
  expect(keys).not.toContain('hold_sha');
});
