// Item 570c66e9, `human_only_by_passkey_20261007` (David, 2026-10-07:
// "human-only step completion should use passkey for enforcement"). The
// jobs API refuses a bare completion of a `human_only` step — the id a
// caller asserts is not proof of the person — answering 422
// {required: "presence", human_only: true}. Nine of the ten plain
// human-only steps on the live registry are `task` or
// `credential-rotation` steps, which render on GenericSurface; its
// Complete was one PUT with no ceremony, so after that rule none of them
// could be completed from the web at all.
//
// GenericSurface now draws what the passkey signs on such a step and
// answers that refusal with ONE tap on the step as shown and ONE retry.
// The mocks below are the server: the answer through the merge door, a
// details PUT that does not complete, a bare completion refused, and a
// completion honoured only on a ticket a ceremony issued.

import { expect, test, type Page, type Route } from './_test';
import { servePeopleRows } from './_smokeMocks';

const JOB_ID = 'job-gho-1';

const EMP = { id: 'emp-001', name: 'David', email: 'd@a', role: 'platform-admin',
  department: 'it', hire_date: '2023-01-01', status: 'active', location: 'loc-hq',
  employment_type: 'full-time', skills: [], certifications: [] };

const json = (r: Route, b: unknown, status = 200) =>
  r.fulfill({ status, contentType: 'application/json', body: JSON.stringify(b) });

const HUMAN_ONLY_REFUSAL = {
  error: 'step requires stronger assurance than this request carries',
  required: 'presence',
  produced: 'session',
  human_only: true,
};

type Put = { ticket: string | undefined; body: Record<string, unknown> };
type Seen = { puts: Put[]; patches: unknown[]; begins: unknown[]; finishes: number; asked: number };

async function humanOnlyTask(page: Page, humanOnly: unknown): Promise<Seen> {
  await page.addInitScript(() => {
    setInterval(() => document.querySelector('bun-hmr')?.remove(), 200);
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    (window as unknown as { __asked: number }).__asked = 0;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: async () => {
          (window as unknown as { __asked: number }).__asked += 1;
          return {
            id: 'cred-1',
            rawId: buf(),
            type: 'public-key',
            response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
              userHandle: null },
          };
        },
      },
    });
  });
  const metadata: Record<string, unknown> = { flag: 'it-map-motion' };
  if (humanOnly !== undefined) metadata.human_only = humanOnly;
  const step = {
    id: 's1', job_id: JOB_ID, title: 'Widen the flight', kind: 'task', spec_slug: 'widen',
    status: 'ready', assignee_id: null as string | null, sort_order: 0, blocked_by: [],
    sign_offs_required: [], sign_offs: [],
    fields: [{ name: 'why', field_type: 'string', required: true }],
    metadata, notes: null,
  };
  const job = {
    id: JOB_ID, kind: 'flight-a-change', title: 'Flight a change', status: 'open',
    opened_on: '2026-10-07', due_on: null, closed_on: null, owner_id: EMP.id,
    priority: 'standard', simulated: false, tags: [],
    subject: { subject_kind: 'custom', id: 'it-map-motion' }, metadata: {},
  };
  const seen: Seen = { puts: [], patches: [], begins: [], finishes: 0, asked: 0 };
  const issued: string[] = [];

  await page.route('**/api/**', (r) => json(r, []));
  await page.route(/\/api\/people$/, (r) => json(r, [EMP]));
  await servePeopleRows(page, [EMP]);
  await page.route(/\/api\/session$/, (r) =>
    json(r, { username: 'david', employee_id: EMP.id, role: 'platform-admin' }));
  await page.route(/\/api\/jobs\/live$/, (r) =>
    json(r, { counts: {}, open_total: 0, recent: [], sim_clock: {} }));
  await page.route(/\/api\/jobs\/step-types$/, (r) => json(r, [
    { kind: 'task', label: 'Task', category: 'work', ux: 'inline', description: '',
      surface: 'generic' },
  ]));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}$`), (r) => json(r, { ...job, steps: [step] }));
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1/metadata$`), (r) => {
    const patch = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
    seen.patches.push(patch);
    Object.assign(step.metadata, patch);
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
  await page.route(new RegExp(`/api/jobs/${JOB_ID}/steps/s1$`), async (r) => {
    const ticket = (await r.request().headerValue('x-presence-ticket')) ?? undefined;
    const body = JSON.parse(r.request().postData() ?? '{}') as Record<string, unknown>;
    seen.puts.push({ ticket, body });
    if (body.status === 'completed') {
      const reserved = step.metadata.human_only === true || step.metadata.human_only === 'true';
      // A completion on a passkey changes nothing beside the status.
      if (reserved && Object.keys(body).length > 1) {
        return json(r, { error: 'a presence-assured step completes the content its ceremony saw' }, 409);
      }
      if (reserved && !(ticket && issued.includes(ticket))) {
        return json(r, HUMAN_ONLY_REFUSAL, 422);
      }
      step.status = 'completed';
    }
    return json(r, step);
  });
  return seen;
}

for (const declared of [true, 'true'] as const) {
  test(`a human-only task (human_only = ${JSON.stringify(declared)}) completes on one passkey tap over what the surface shows`, async ({ page }) => {
    const seen = await humanOnlyTask(page, declared);

    await page.goto(`/ux/jobs/${JOB_ID}`);
    const surface = page.locator('.sg-detail');
    // What the passkey would sign is on screen before anything is pressed.
    const block = surface.locator('.step-signed-keys');
    await expect(block).toContainText('What your passkey signs');
    await expect(block).toContainText('human_only');
    await expect(block).toContainText('it-map-motion');

    await surface.locator('.step-ask textarea').fill('every cohort is clean');
    await surface.locator('.step-ask').getByRole('button', { name: 'Complete' }).click();

    await expect(surface.locator('.step-status')).toHaveText('completed');
    await expect(surface.locator('.step-write-error')).toHaveCount(0);
    expect(seen.patches).toEqual([{ why: 'every cohort is clean' }]);
    // One tap; the ticket rides the retry alone, and the completing
    // requests carry the status and nothing else.
    expect(seen.finishes).toBe(1);
    expect(await page.evaluate(() => (window as unknown as { __asked: number }).__asked)).toBe(1);
    const completions = seen.puts.filter((p) => p.body.status === 'completed');
    expect(completions).toEqual([
      { ticket: undefined, body: { status: 'completed' } },
      { ticket: 'ticket-1', body: { status: 'completed' } },
    ]);
    expect(seen.puts.filter((p) => p.body.status !== 'completed').every((p) => !p.ticket)).toBe(true);
    // The tap signs the step as shown, the answer folded in.
    const shown = (seen.begins[0] as { shown: { title: string; metadata: Record<string, unknown> } })
      .shown;
    expect(shown.title).toBe('Widen the flight');
    expect(shown.metadata).toEqual({
      flag: 'it-map-motion',
      human_only: declared,
      why: 'every cohort is clean',
    });
  });
}

test('control: a task that is not human-only completes as before, with no passkey and no signing block', async ({ page }) => {
  const seen = await humanOnlyTask(page, undefined);

  await page.goto(`/ux/jobs/${JOB_ID}`);
  const surface = page.locator('.sg-detail');
  await expect(surface.locator('.step-ask')).toBeVisible();
  await expect(surface.locator('.step-signed-keys')).toHaveCount(0);
  await surface.locator('.step-ask textarea').fill('done');
  await surface.locator('.step-ask').getByRole('button', { name: 'Complete' }).click();

  await expect(surface.locator('.step-status')).toHaveText('completed');
  expect(seen.begins).toEqual([]);
  expect(seen.finishes).toBe(0);
  expect(seen.puts.filter((p) => p.body.status === 'completed').length).toBe(1);
});
