// A rendered plan files its mutating request from its own page (backlog
// 7aef2405, David 2026-11-05: "None of those steps are actually ready for
// me though, right?"). Acting on the disk_tight alarm's rendered rebuild
// plan took a terminal command; the page now offers "File the request",
// which POSTs to the jobs API's file-the-request door with NO body — the
// write, its args and its flags are read off the plan by the server, and
// no filer supplies plan_sha256 — and then links the request filed, which
// waits on its approve step for the passkey. Filing is not approving.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const PLAN = 'plan-rebuild-1';
const FILED = 'req-rebuild-1';

const OPERATOR = {
  id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [],
};

const REQUEST = {
  host: 'forge',
  verb: 'rebuild-a-failed-replica',
  args: ['pvc-0a1b2c', 'pvc-0a1b2c-r-1234abcd'],
  title: 'rebuild-a-failed-replica pvc-0a1b2c pvc-0a1b2c-r-1234abcd on forge',
  mutating: true,
  requires_approval: true,
  requires_review: false,
  approvers: ['emp-david'],
};

const json = (r: Route, b: unknown, status = 200): Promise<void> =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const execute = {
  id: 's-exec', job_id: PLAN, kind: 'task', title: 'execute', status: 'completed',
  assignee_id: null, sort_order: 1, blocked_by: [], sign_offs_required: [], sign_offs: [],
  completed_on: '2026-11-05', notes: null, spec_slug: 'execute',
  metadata: { disposition: 'answered', exit_code: '0', output: 'PLAN rebuild pvc-0a1b2c', runner_host: 'forge' },
};

type Posted = { method: string; body: string | null };

/** The plan packet's page, with the door answering as told. */
async function mocks(
  page: Page,
  verb: string,
  door: { preview: unknown; after?: unknown; post?: { status: number; body: unknown } },
): Promise<Posted[]> {
  const posted: Posted[] = [];
  let filed = false;
  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(new RegExp(`/api/jobs/${PLAN}$`), (r) => json(r, {
    id: PLAN, kind: 'ops-request', title: `${verb} pvc-0a1b2c pvc-0a1b2c-r-1234abcd on forge`,
    status: 'closed', opened_on: '2026-11-05', due_on: null, closed_on: '2026-11-05',
    owner_id: 'emp-david', priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' },
    metadata: { host: 'forge', verb, args: ['pvc-0a1b2c', 'pvc-0a1b2c-r-1234abcd'] },
    steps: [execute],
  }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'generic', ux: 'inline', description: '' },
  ]));
  await page.route(/\/api\/people$/, (r) => json(r, [OPERATOR]));
  await servePeopleRows(page, [OPERATOR]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: OPERATOR.id, role: OPERATOR.role }));
  await page.route(new RegExp(`/api/jobs/${PLAN}/file-the-request$`), (r) => {
    const req = r.request();
    if (req.method() === 'POST') {
      posted.push({ method: 'POST', body: req.postData() });
      const answer = door.post ?? { status: 201, body: { id: FILED } };
      if (answer.status < 300) filed = true;
      return json(r, answer.body, answer.status);
    }
    return json(r, filed && door.after !== undefined ? door.after : door.preview);
  });
  return posted;
}

test('a clean plan files its write as the viewer, with no body, and links the request', async ({ page }) => {
  const posted = await mocks(page, 'plan-a-failed-replica-rebuild', {
    preview: { plan: true, fileable: true, write: REQUEST.verb, request: REQUEST, open_request: null },
    after: {
      plan: true, fileable: false, write: REQUEST.verb, request: REQUEST, open_request: FILED,
      why: `a request filed from this plan is still open: ${FILED}.`,
    },
  });
  await page.goto(`/ux/jobs/${PLAN}`);

  const panel = page.getByTestId('file-the-request');
  await expect(panel).toBeVisible();
  await expect(panel).toContainText(REQUEST.title);
  await expect(panel).toContainText('It waits on a passkey approval by emp-david.');
  await expect(panel).toContainText('Filing is not approving.');
  await panel.getByRole('button', { name: 'File the request' }).click();

  await expect.poll(() => posted.length).toBe(1);
  expect(posted[0]?.body ?? null).toBeNull();
  const link = panel.getByRole('link', { name: FILED });
  await expect(link).toBeVisible();
  await expect(link).toHaveAttribute('href', new RegExp(`/jobs/${FILED}$`));
  await expect(panel.getByRole('button', { name: 'File the request' })).toHaveCount(0);
});

test('a refused filing is shown verbatim, and nothing is linked', async ({ page }) => {
  const refusal = 'opening a ops-request packet needs Create on job:ops-request (or on job, every kind): no rule';
  await mocks(page, 'plan-a-failed-replica-rebuild', {
    preview: { plan: true, fileable: true, write: REQUEST.verb, request: REQUEST, open_request: null },
    post: { status: 403, body: refusal },
  });
  await page.goto(`/ux/jobs/${PLAN}`);

  const panel = page.getByTestId('file-the-request');
  await panel.getByRole('button', { name: 'File the request' }).click();
  await expect(panel.getByRole('alert')).toContainText('HTTP 403');
  await expect(panel.getByRole('alert')).toContainText('needs Create on job:ops-request');
  await expect(panel.getByRole('link', { name: FILED })).toHaveCount(0);
});

test('a plan that did not render clean says why and offers nothing', async ({ page }) => {
  const posted = await mocks(page, 'plan-a-failed-replica-rebuild', {
    preview: {
      plan: true, fileable: false, write: REQUEST.verb, open_request: null,
      why: 'the plan did not render clean: the runner answered it with exit 78',
    },
  });
  await page.goto(`/ux/jobs/${PLAN}`);

  const panel = page.getByTestId('file-the-request');
  await expect(panel).toContainText('rebuild-a-failed-replica cannot be filed from this plan');
  await expect(panel).toContainText('exit 78');
  await expect(panel.getByRole('button', { name: 'File the request' })).toHaveCount(0);
  expect(posted).toEqual([]);
});

test('a packet the server reads as no plan draws no panel', async ({ page }) => {
  await mocks(page, 'uptime', { preview: { plan: false, why: '`uptime` is the plan_verb of no allowlisted write' } });
  // The absence is read only after the server's answer has landed: an
  // absence read before it would pass on a page that never asked.
  const answered = page.waitForResponse((r) => r.url().endsWith(`/api/jobs/${PLAN}/file-the-request`));
  await page.goto(`/ux/jobs/${PLAN}`);
  await answered;
  await expect(page.getByText('uptime pvc-0a1b2c pvc-0a1b2c-r-1234abcd on forge').first()).toBeVisible();
  await expect(page.getByTestId('file-the-request')).toHaveCount(0);
});
