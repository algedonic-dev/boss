// /jobs/<short id> loads the packet (backlog 434b6e0d). The post-mortem
// archive links a finding's packet refs, and those refs are the eight
// characters every surface prints (`21d54f4a remove the push mirror`).
// The detail READ resolves such a prefix (cd7b0054, 2026-09-21), but the
// page opened its live stream first, and the stream parses a full uuid
// only: it answers a short id with one `error` frame and ends, the
// browser reconnects, and the page never reached the read that would
// have answered — "Loading…" for good. The stream below answers the way
// the live one does (boss-jobs http/jobs.rs `job_stream`).

import { expect, test, type Route } from './_test';
import { installApiFloor } from './_smokeMocks';

const json = (r: Route, body: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });

const SHORT = '21d54f4a';
const FULL = '21d54f4a-0000-4000-8000-000000000001';

const JOB = {
  id: FULL, kind: 'backlog-item', title: 'Remove the push mirror', status: 'open',
  subject: { subject_kind: 'account', id: 'a1' },
  owner_id: 'emp-david', priority: 'standard', opened_on: '2026-09-26',
  due_on: null, closed_on: null, metadata: {}, tags: [], steps: [],
};

test('a short id renders its packet through the read, not a stream that cannot parse it', async ({ page }) => {
  await installApiFloor(page);
  await page.route(new RegExp(`/api/jobs/${SHORT}/stream$`), (r) =>
    r.fulfill({
      status: 200,
      contentType: 'text/event-stream',
      body: 'event: error\ndata: invalid job id\n\n',
    }));
  await page.route(new RegExp(`/api/jobs/${SHORT}$`), (r) => json(r, JOB));

  await page.goto(`/jobs/${SHORT}`);

  await expect(page.locator('h1')).toContainText('Remove the push mirror');
});

// The step surfaces write with the LOADED job's id, not the route's
// (backlog 97f98011). Once the short id loaded, every step write on the
// page went out as `/api/jobs/<8 hex>/steps/…`, and the step doors
// parsed a full uuid only: David's ratify of doc-flatten 06170f1c was
// refused `400 invalid job id` (step-write refusal row 27765). The
// server now resolves the prefix too; the page still sends the id it
// read, so a write names exactly the packet the reader is looking at.
const STEP_ID = 'ab8036d5-0326-46be-9585-90c4636d9116';
const ACTIVE_STEP = {
  id: STEP_ID, job_id: FULL, kind: 'task', title: 'Build the change',
  assignee_id: 'emp-bootstrap-admin', status: 'active', sort_order: 1,
  blocked_by: [], sign_offs_required: [], sign_offs: [], completed_on: null, notes: null,
  fields: [{ name: 'disposition', field_type: 'stale|delivered', required: true }],
  metadata: { authority_role: 'platform-admin' },
};
const ADMIN = {
  id: 'emp-bootstrap-admin', name: 'Bootstrap Admin', email: 'admin@boss',
  role: 'platform-admin', department: 'it', hire_date: '2023-01-01',
  status: 'active', location: 'hq', employment_type: 'full-time',
  skills: [], certifications: [],
};

test('a step completed on a short-id page writes to the full id it loaded', async ({ page }) => {
  await installApiFloor(page);
  await page.route(/\/api\/people$/, (r) => json(r, [ADMIN]));
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: ADMIN.email, employee_id: ADMIN.id, role: ADMIN.role }));
  await page.route(new RegExp(`/api/jobs/${SHORT}/stream$`), (r) =>
    r.fulfill({
      status: 200,
      contentType: 'text/event-stream',
      body: 'event: error\ndata: invalid job id\n\n',
    }));
  await page.route(new RegExp(`/api/jobs/${SHORT}$`), (r) =>
    json(r, { ...JOB, steps: [ACTIVE_STEP] }));

  // Every step-door URL the page sends, whatever id it carries.
  const sent: string[] = [];
  await page.route(/\/api\/jobs\/[^/]+\/steps\/[^/]+(\/metadata)?$/, async (route) => {
    const method = route.request().method();
    if (method !== 'PUT' && method !== 'PATCH') return route.fallback();
    sent.push(`${method} ${new URL(route.request().url()).pathname}`);
    return json(route, ACTIVE_STEP);
  });

  await page.goto(`/jobs/${SHORT}`);
  await expect(page.locator('h1')).toContainText('Remove the push mirror');

  await page.getByLabel(/disposition/i).selectOption('delivered');
  await page.getByRole('button', { name: /^complete$/i }).click();

  await expect.poll(() => sent.some((s) => s.startsWith('PUT '))).toBe(true);
  expect(sent).toContain(`PATCH /api/jobs/${FULL}/steps/${STEP_ID}/metadata`);
  expect(sent).toContain(`PUT /api/jobs/${FULL}/steps/${STEP_ID}`);
  expect(sent.filter((s) => s.includes(`/api/jobs/${SHORT}/`))).toEqual([]);
});
