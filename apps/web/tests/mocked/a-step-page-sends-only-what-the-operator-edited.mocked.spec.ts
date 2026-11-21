// Backlog ec1e1980 (found by review run 13c5b930, 2026-10-20). The
// generic step surface seeded every declared field's box once, when the
// page was drawn, and every Save, Start and Complete re-sent each
// non-empty box through the merge door, edited or not. So a page left
// open reverted any declared key another writer had changed since it was
// drawn — a reviewer, the conductor, a door — and the audit log credited
// the operator with that write. The server's guard
// (field_writer::strip_unchanged_reserved) covers only a field that
// declares a `writer`; a plain declared field had none.
//
// Now the page sends only the fields the operator edited on it, measured
// against what it last knew the record held. The mocks below are the
// server: one step whose metadata the merge door really merges into, so
// a write the page should not have made shows up as a reverted value,
// not only as a key in a request body.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-left-open-1';

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

type Merge = { from: 'page' | 'other writer'; body: Record<string, unknown> };
type Server = {
  step: {
    id: string; status: string; assignee_id: string | null;
    metadata: Record<string, unknown>;
  } & Record<string, unknown>;
  merges: Merge[];
};

/// The page around one READY step with two declared string fields:
/// `finding`, already answered when the page is drawn, and `note`,
/// empty. The merge door merges (a null deletes, as the real one does);
/// the claim door makes the step active.
async function server(page: Page): Promise<Server> {
  const s: Server = {
    step: {
      id: 's1', job_id: JOB_ID, kind: 'task', title: 'review the finding', status: 'ready',
      assignee_id: null, sort_order: 0, blocked_by: [], sign_offs_required: [],
      sign_offs: [], completed_on: null, notes: null, spec_slug: 'review',
      metadata: { finding: 'as first drawn' },
      fields: [
        { name: 'finding', field_type: 'string' },
        { name: 'note', field_type: 'string' },
      ],
    },
    merges: [],
  };
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, {
    id: JOB_ID, kind: 'review-a-finding', title: 'A finding',
    status: 'open', opened_on: '2026-10-20', due_on: null, closed_on: null,
    owner_id: OPERATOR.id, priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'finding-1' }, metadata: {},
    steps: [structuredClone(s.step)],
  }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR]));
  await servePeopleRows(page, [OPERATOR]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1(/metadata|/claim)?(\\?.*)?$`), (r) => {
    const req = r.request();
    const url = new URL(req.url());
    if (url.pathname.endsWith('/metadata')) {
      const body = req.postDataJSON() as Record<string, unknown>;
      s.merges.push({ from: req.headers()['x-test-writer'] ? 'other writer' : 'page', body });
      for (const [k, v] of Object.entries(body)) {
        if (v === null) delete s.step.metadata[k];
        else s.step.metadata[k] = v;
      }
      return json(r, s.step);
    }
    if (url.pathname.endsWith('/claim')) {
      s.step.status = 'active';
      s.step.assignee_id = OPERATOR.id;
      return json(r, s.step);
    }
    const body = (req.postData() ? req.postDataJSON() : {}) as Record<string, unknown>;
    if (body['status'] === 'completed') s.step.status = 'completed';
    return json(r, s.step);
  });
  return s;
}

/// Another writer changes a declared key through the merge door after
/// the page was drawn. The page is not told: it is a page left open.
async function otherWriterSets(page: Page, patch: Record<string, unknown>): Promise<void> {
  const status = await page.evaluate(async ([job, body]) => {
    const r = await fetch(`/api/jobs/${job}/steps/s1/metadata`, {
      method: 'PATCH',
      headers: { 'content-type': 'application/json', 'x-test-writer': 'reviewer' },
      body: JSON.stringify(body),
    });
    return r.status;
  }, [JOB_ID, patch] as const);
  expect(status).toBe(200);
}

const fromPage = (s: Server): Record<string, unknown>[] =>
  s.merges.filter((m) => m.from === 'page').map((m) => m.body);

test('a page left open does not revert a declared key another writer changed when Start is pressed', async ({ page }) => {
  const s = await server(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.step-generic');
  await expect(surface.locator('[data-field="finding"] textarea')).toHaveValue('as first drawn');

  await otherWriterSets(page, { finding: 'the reviewer\'s newer finding' });
  await surface.getByRole('button', { name: 'Start' }).click();

  await expect.poll(() => s.step.status).toBe('active');
  expect(s.step.metadata['finding'], 'the newer write stands').toBe('the reviewer\'s newer finding');
  expect(fromPage(s), 'the page wrote no declared key the operator did not edit').toEqual([]);
});

test('Start sends the field the operator edited, and only that one', async ({ page }) => {
  const s = await server(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.step-generic');
  await expect(surface.locator('[data-field="finding"] textarea')).toHaveValue('as first drawn');

  await otherWriterSets(page, { finding: 'the reviewer\'s newer finding' });
  await surface.locator('[data-field="note"] textarea').fill('checked the fixture');
  await surface.getByRole('button', { name: 'Start' }).click();

  await expect.poll(() => s.step.status).toBe('active');
  expect(fromPage(s)).toEqual([{ note: 'checked the fixture' }]);
  expect(s.step.metadata).toEqual({
    finding: 'the reviewer\'s newer finding',
    note: 'checked the fixture',
  });
});

test('an answer the page already sent is not sent again over a newer write', async ({ page }) => {
  const s = await server(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.step-generic');
  await expect(surface.locator('[data-field="finding"] textarea')).toHaveValue('as first drawn');

  await surface.locator('[data-field="note"] textarea').fill('checked the fixture');
  await surface.getByRole('button', { name: 'Start' }).click();
  await expect.poll(() => s.step.status).toBe('active');
  await expect(surface.locator('.step-status')).toHaveText('active');

  // The operator's own answer landed; then someone else rewrites it.
  await otherWriterSets(page, { note: 'the conductor\'s correction' });
  await surface.locator('.step-ask').getByRole('button', { name: 'Complete' }).click();

  await expect.poll(() => s.step.status).toBe('completed');
  expect(fromPage(s), 'Start sent the answer once; Complete sends nothing it did not edit')
    .toEqual([{ note: 'checked the fixture' }]);
  expect(s.step.metadata['note']).toBe('the conductor\'s correction');
});

test('an untouched box follows the record when the page re-reads the step', async ({ page }) => {
  const s = await server(page);
  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.step-generic');
  await expect(surface.locator('[data-field="finding"] textarea')).toHaveValue('as first drawn');

  await otherWriterSets(page, { finding: 'the reviewer\'s newer finding' });
  // Start re-reads the job; the box then shows what the record holds.
  await surface.getByRole('button', { name: 'Start' }).click();
  await expect(surface.locator('.step-status')).toHaveText('active');
  await expect(surface.locator('[data-field="finding"] textarea'))
    .toHaveValue('the reviewer\'s newer finding');
});
