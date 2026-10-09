// Backlog 3ce3c15f (review of car 5b30ccf9, 2026-09-25): a completion the
// jobs API refuses for PRESENCE stopped ApprovalSurface at the raw 422.
//
// Since design 1ce67f7e (2026-10-07) that refusal — {required:
// "presence"} — has one source: a presence step that names NO sign-off
// role, whose completion is its only act and so carries the ticket
// itself. (A step that names roles completes on its stamps with a bare
// request; the expired-ticket shape this file once covered is gone with
// the ticket the surface used to hold.) The surface answers it with ONE
// passkey tap on the step as shown and retries the completion ONCE with
// the fresh ticket. The mocks below issue a distinct ticket per ceremony
// and honour only the ones they say, so a surface that looped or invented
// a ticket cannot pass.
//
// And a step whose STAMPS do not carry it is refused without that key,
// naming the roles and the reason: no tap is taken for it.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-apr-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const PRESENCE_REFUSAL = {
  error: 'step requires stronger assurance than this request carries',
  required: 'presence',
  produced: 'session',
};

type Seen = { stampTickets: (string | undefined)[]; putTickets: (string | undefined)[];
  begins: unknown[]; finishes: number };

type Setup = Readonly<{
  signOffsRequired: string[];
  signOffs: unknown[];
  /** Which tickets the completion honours, given every ticket issued so far. */
  completionHonours: (ticket: string | undefined, issued: readonly string[]) => boolean;
  /** What a completion not honoured is answered with (default: the presence refusal). */
  completionRefusal?: unknown;
}>;

async function presenceGatedApproval(page: Page, setup: Setup): Promise<Seen> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
    // The authenticator: a passkey that signs whatever it is asked to.
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: async () => ({
          id: 'cred-1',
          rawId: buf(),
          type: 'public-key',
          response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
            userHandle: null },
        }),
      },
    });
  });
  const step = {
    id: 's1', job_id: JOB_ID, title: 'Approve the plan: wipe on forge', kind: 'sign-off',
    status: 'ready', assignee_id: null, sort_order: 0, blocked_by: [],
    sign_offs_required: setup.signOffsRequired, sign_offs: setup.signOffs,
    assurance_required: 'presence',
    metadata: { plan: 'PLAN wipe target-a' } as Record<string, unknown>, notes: null,
  };
  const job = {
    id: JOB_ID, kind: 'ops-request', title: 'wipe a disk on forge', status: 'open',
    opened_on: '2026-09-25', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'forge' }, metadata: {},
  };
  const seen: Seen = { stampTickets: [], putTickets: [], begins: [], finishes: 0 };
  const issued: string[] = [];

  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'sign-off', label: 'Sign-off', category: 'approval', ux: 'inline',
      description: '', surface: 'approval' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps: [step] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/metadata$`), (r) => {
    Object.assign(step.metadata, JSON.parse(r.request().postData() ?? '{}'));
    return json(r, step);
  });
  await page.route(/\/api\/auth\/passkey\/assert\/begin$/, (r) => {
    seen.begins.push(JSON.parse(r.request().postData() ?? '{}'));
    return json(r, {
      challenge_id: `chal-${seen.begins.length}`,
      publicKey: { challenge: 'AAAA', rpId: 'localhost',
        allowCredentials: [{ type: 'public-key', id: 'AAAA' }],
        userVerification: 'required', timeout: 60000 },
    });
  });
  await page.route(/\/api\/auth\/passkey\/assert\/finish$/, (r) => {
    seen.finishes += 1;
    const ticket = `ticket-${seen.finishes}`;
    issued.push(ticket);
    return json(r, { ticket });
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/sign-offs$`), async (r) => {
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    seen.stampTickets.push(ticket);
    if (!ticket || !issued.includes(ticket)) return json(r, PRESENCE_REFUSAL, 422);
    step.sign_offs = [{ role: 'platform-admin', authority_id: EMP.id, shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n' }];
    return json(r, step);
  });
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), async (r) => {
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    seen.putTickets.push(ticket);
    if (!setup.completionHonours(ticket, issued)) {
      return json(r, setup.completionRefusal ?? PRESENCE_REFUSAL, 422);
    }
    step.status = 'completed';
    return json(r, step);
  });
  return seen;
}

test('a presence step with no sign-off role: Approve takes one tap and completes', async ({ page }) => {
  // The step names no role, so no stamp ceremony runs and the server
  // judges the completion on its own ticket: it honours only one a
  // ceremony issued. (The stray stamp stands for nothing — no role asked
  // for it.)
  const seen = await presenceGatedApproval(page, {
    signOffsRequired: [],
    signOffs: [{ role: 'controller', authority_id: 'emp-002', shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n0' }],
    completionHonours: (t, issued) => t !== undefined && issued.includes(t),
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-status')).toHaveText('completed');
  await expect(surface.locator('.step-write-error')).toHaveCount(0);
  expect(seen.stampTickets).toEqual([]);
  expect(seen.finishes).toBe(1);
  expect(seen.putTickets).toEqual([undefined, 'ticket-1']);
  // The tap signs the step as this surface showed it, decision folded in.
  const shown = (seen.begins[0] as { shown: { title: string; metadata: Record<string, unknown> } })
    .shown;
  expect(shown.title).toBe('Approve the plan: wipe on forge');
  expect(shown.metadata.plan).toBe('PLAN wipe target-a');
  expect(shown.metadata.decision).toBe('approved');
});

test('stamps that do not carry the step: the roles and the reason are shown, and no tap is taken', async ({ page }) => {
  // Another role's signature is past its age. A ticket on the completion
  // would change nothing, so the refusal carries no `required: presence`
  // and the surface runs no ceremony for it.
  const seen = await presenceGatedApproval(page, {
    signOffsRequired: ['controller'],
    signOffs: [{ role: 'controller', authority_id: 'emp-002', shape_hash: 'h',
      assurance: 'presence', presence_nonce: 'n0' }],
    completionHonours: () => false,
    completionRefusal: {
      error: 'this step completes on its sign-off stamps, and they do not carry it',
      completes_on: 'stamps',
      missing_or_stale_roles: ['controller'],
      detail: 'role controller was signed by emp-002 more than 72 hours ago',
    },
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-write-error')).toContainText(
    'sign-offs outstanding: controller — role controller was signed by emp-002 more than 72 hours ago',
  );
  expect(seen.stampTickets).toEqual([]);
  expect(seen.begins).toEqual([]);
  expect(seen.finishes).toBe(0);
  expect(seen.putTickets).toEqual([undefined]);
});

test('refused again after the fresh tap: the surface says so and never asks a third time', async ({ page }) => {
  const seen = await presenceGatedApproval(page, {
    signOffsRequired: [],
    signOffs: [],
    completionHonours: () => false,
  });

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await surface.getByRole('button', { name: 'Approve' }).click();

  await expect(surface.locator('.step-write-error')).toContainText(
    'refused again after a fresh passkey tap',
  );
  expect(seen.finishes).toBe(1);
  expect(seen.putTickets).toEqual([undefined, 'ticket-1']);
});
