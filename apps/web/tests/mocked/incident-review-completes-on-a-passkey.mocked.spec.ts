// Item 570c66e9, `human_only_by_passkey_20261007` (David, 2026-10-07:
// "human-only step completion should use passkey for enforcement"). The
// incident `review` step is human_only with no sign-off role, so the
// jobs API completes it only on the reviewer's own passkey ticket — and
// infra/step-plugins/incident-review.js completed with one bare PUT.
// Left that way, the next incident's review could have been finished by
// no surface at all.
//
// The actual bundles are driven here through the host contract, as
// step-plugin-save-refusals does: the shared passkey-ceremony.js first
// (the one copy sign-off.js uses too), then incident-review.js. The
// in-page fetch is the server: the packet read, a bare completion of a
// human-only step refused 422 {required: "presence", human_only: true},
// and a completion honoured only on a ticket a ceremony issued.

import { readFileSync } from 'node:fs';
import { expect, test, type Page } from './_test';

const bundle = (name: string) =>
  readFileSync(new URL(`../../../../infra/step-plugins/${name}`, import.meta.url), 'utf8');

type Call = { method: string; url: string; ticket: string | null; body: Record<string, unknown> | null };
type State = {
  calls: Call[];
  updates: number;
  asked: number;
  passkey: 'gives' | 'refuses';
  step: { status: string; title: string; metadata: Record<string, unknown> };
  props: { step: Record<string, unknown> };
};

type Setup = Readonly<{
  metadata: Record<string, unknown>;
  /** The server asks a passkey of the completion whatever the step declares. */
  serverAsks?: boolean;
  /** Leave the shared ceremony file out, as a delivery that lost it would. */
  withoutCeremony?: boolean;
}>;

async function mount(page: Page, setup: Setup): Promise<string[]> {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.setContent('<div id="host"></div>');
  await page.evaluate((setup: Setup) => {
    const w = window as unknown as Record<string, unknown>;
    const row = { status: 'ready', title: 'Human review of the findings',
      metadata: structuredClone(setup.metadata) as Record<string, unknown> };
    const state: State = { calls: [], updates: 0, asked: 0, passkey: 'gives', step: row,
      props: { step: {} } };
    w['__state'] = state;
    w['__boss_register_step_plugin'] = (_kind: string, fn: unknown) => { w['__mount'] = fn; };
    const buf = () => new Uint8Array([1, 2, 3]).buffer;
    Object.defineProperty(navigator, 'credentials', {
      configurable: true,
      value: {
        get: async () => {
          state.asked += 1;
          if (state.passkey === 'refuses') {
            throw new DOMException('The operation either timed out or was not allowed.', 'NotAllowedError');
          }
          return { id: 'cred-1', rawId: buf(), type: 'public-key',
            response: { authenticatorData: buf(), clientDataJSON: buf(), signature: buf(),
              userHandle: null } };
        },
      },
    });
    const issued: string[] = [];
    const json = (b: unknown, status = 200) =>
      new Response(JSON.stringify(b), { status, headers: { 'content-type': 'application/json' } });
    w['fetch'] = async (url: string, options?: RequestInit) => {
      const method = options?.method ?? 'GET';
      const ticket = new Headers(options?.headers).get('x-presence-ticket');
      const body = options?.body ? (JSON.parse(String(options.body)) as Record<string, unknown>) : null;
      state.calls.push({ method, url, ticket, body });
      if (method === 'GET' && url === '/api/jobs/job') {
        return json({ id: 'job', title: 'Jobs API dark for four hours',
          metadata: { summary: 'A build bricked the boot.' }, steps: [] });
      }
      if (url.endsWith('/assert/begin')) {
        return json({ challenge_id: 'chal', publicKey: { challenge: 'AAAA', rpId: 'localhost',
          allowCredentials: [{ type: 'public-key', id: 'AAAA' }], userVerification: 'required',
          timeout: 60000 } });
      }
      if (url.endsWith('/assert/finish')) {
        const t = `ticket-${issued.length + 1}`;
        issued.push(t);
        return json({ ticket: t });
      }
      if (method === 'PUT' && url === '/api/jobs/job/steps/step') {
        const reserved = row.metadata['human_only'] === true || row.metadata['human_only'] === 'true';
        if ((reserved || setup.serverAsks) && !(ticket && issued.includes(ticket))) {
          return json({ error: 'step requires stronger assurance than this request carries',
            required: 'presence', produced: 'session', human_only: reserved }, 422);
        }
        row.status = String(body?.['status']);
        return new Response(null, { status: 204 });
      }
      throw new Error(`unexpected door: ${method} ${url}`);
    };
  }, setup);
  if (!setup.withoutCeremony) await page.addScriptTag({ content: bundle('passkey-ceremony.js') });
  await page.addScriptTag({ content: bundle('incident-review.js') });
  await expect.poll(() => page.evaluate(() => typeof (window as unknown as Record<string, unknown>)['__mount'])).toBe('function');
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const state = w['__state'] as State;
    const fn = w['__mount'] as (host: Element, args: unknown) => void;
    state.props.step = { id: 'step', kind: 'incident-review', title: state.step.title,
      status: 'ready', assignee_id: null, sort_order: 9, sign_offs_required: [],
      metadata: structuredClone(state.step.metadata), notes: null };
    fn(document.getElementById('host')!, { step: state.props.step, jobId: 'job',
      onUpdate() { state.updates++; } });
  });
  return errors;
}

const state = (page: Page) =>
  page.evaluate(() => (window as unknown as { __state: State }).__state);
const COMPLETE = 'Findings reviewed — complete review';
const writes = (s: State) =>
  s.calls.filter((c) => c.method !== 'GET').map((c) => `${c.method} ${c.url} ${c.ticket ?? '-'}`);

for (const declared of [true, 'true'] as const) {
  test(`the human-only review (human_only = ${JSON.stringify(declared)}) completes on one passkey tap over what the surface drew`, async ({ page }) => {
    const errors = await mount(page, { metadata: { human_only: declared, authority_role: 'platform-admin' } });
    // What the passkey would sign is drawn before anything is pressed.
    const block = page.locator('.step-signed-keys');
    await expect(block).toContainText('What your passkey signs');
    await expect(block.locator('.step-signed-title')).toHaveText('Human review of the findings');
    await expect(block.locator('.step-signed-key')).toHaveText(['authority_role', 'human_only']);
    await expect(page.locator('.sir-body').first()).toHaveText('A build bricked the boot.');

    await page.getByRole('button', { name: COMPLETE, exact: true }).click();
    await expect.poll(async () => (await state(page)).updates).toBe(1);

    const s = await state(page);
    expect(s.step.status).toBe('completed');
    expect(s.asked).toBe(1);
    // Bare first; the ticket rides the one retry; nothing else is written.
    expect(writes(s)).toEqual([
      'PUT /api/jobs/job/steps/step -',
      'POST /api/auth/passkey/assert/begin -',
      'POST /api/auth/passkey/assert/finish -',
      'PUT /api/jobs/job/steps/step ticket-1',
    ]);
    const puts = s.calls.filter((c) => c.method === 'PUT');
    expect(puts.map((c) => c.body)).toEqual([{ status: 'completed' }, { status: 'completed' }]);
    // The tap signs the step as drawn.
    const begin = s.calls.find((c) => c.url.endsWith('/assert/begin'))!;
    expect(begin.body).toEqual({ job_id: 'job', step_id: 'step', shown: {
      title: 'Human review of the findings',
      metadata: { human_only: declared, authority_role: 'platform-admin' } } });
    expect(errors).toEqual([]);
  });
}

test('a passkey refused or cancelled leaves the review open and says so; pressing again completes it', async ({ page }) => {
  const errors = await mount(page, { metadata: { human_only: true } });
  await page.evaluate(() => { (window as unknown as { __state: State }).__state.passkey = 'refuses'; });
  const button = page.getByRole('button', { name: COMPLETE, exact: true });
  await button.click();

  const err = page.locator('.sir-err');
  await expect(err).toContainText('Completing the review needs your passkey, and it was not given');
  await expect(err).toContainText('The step is still open; nothing was completed.');
  let s = await state(page);
  expect(s.step.status).toBe('ready');
  expect(s.updates).toBe(0);
  expect(s.asked).toBe(1);
  // The completion was not re-sent, and no ticket was ever issued.
  expect(writes(s)).toEqual([
    'PUT /api/jobs/job/steps/step -',
    'POST /api/auth/passkey/assert/begin -',
  ]);
  await expect(button).toBeEnabled();

  await page.evaluate(() => { (window as unknown as { __state: State }).__state.passkey = 'gives'; });
  await button.click();
  await expect.poll(async () => (await state(page)).updates).toBe(1);
  s = await state(page);
  expect(s.step.status).toBe('completed');
  expect(errors).toEqual([]);
});

test('content that changed between the draw and the tap refuses before any request, and is drawn to be read', async ({ page }) => {
  const errors = await mount(page, { metadata: { human_only: true } });
  await expect(page.locator('.step-signed-key')).toHaveText(['human_only']);
  // The step this mount holds changes after it was drawn.
  await page.evaluate(() => {
    const s = (window as unknown as { __state: State }).__state;
    (s.props.step['metadata'] as Record<string, unknown>)['finding'] = 'planted after the draw';
  });
  const button = page.getByRole('button', { name: COMPLETE, exact: true });
  await button.click();

  await expect(page.locator('.sir-err')).toContainText(
    'Nothing was signed or sent: finding changed on this step after this page drew it',
  );
  let s = await state(page);
  expect(writes(s)).toEqual([]);
  expect(s.asked).toBe(0);
  expect(s.step.status).toBe('ready');
  // It is on screen now, for the reviewer to read before pressing again.
  await expect(page.locator('.step-signed-key')).toHaveText(['finding', 'human_only']);
  await expect(page.locator('.step-signed-value').first()).toHaveText('planted after the draw');

  await button.click();
  await expect.poll(async () => (await state(page)).updates).toBe(1);
  s = await state(page);
  const begin = s.calls.find((c) => c.url.endsWith('/assert/begin'))!;
  expect((begin.body as { shown: { metadata: unknown } }).shown.metadata).toEqual({
    human_only: true, finding: 'planted after the draw' });
  expect(errors).toEqual([]);
});

test('control: a review that is not human-only draws no signing block and completes with one bare PUT', async ({ page }) => {
  const errors = await mount(page, { metadata: { authority_role: 'platform-admin' } });
  await expect(page.getByRole('button', { name: COMPLETE, exact: true })).toBeVisible();
  await expect(page.locator('.step-signed-keys')).toHaveCount(0);
  await page.getByRole('button', { name: COMPLETE, exact: true }).click();
  await expect.poll(async () => (await state(page)).updates).toBe(1);
  const s = await state(page);
  expect(writes(s)).toEqual(['PUT /api/jobs/job/steps/step -']);
  expect(s.asked).toBe(0);
  expect(errors).toEqual([]);
});

test('a passkey asked of a step that drew no signing block signs nothing: the block is drawn, and the next press signs it', async ({ page }) => {
  const errors = await mount(page, { metadata: { authority_role: 'platform-admin' }, serverAsks: true });
  await expect(page.locator('.step-signed-keys')).toHaveCount(0);
  const button = page.getByRole('button', { name: COMPLETE, exact: true });
  await button.click();
  await expect(page.locator('.sir-err')).toContainText(
    'nothing was signed: your passkey would sign title, authority_role, which this page had not shown — it is shown now',
  );
  let s = await state(page);
  expect(s.asked).toBe(0);
  expect(writes(s)).toEqual(['PUT /api/jobs/job/steps/step -']);
  await expect(page.locator('.step-signed-key')).toHaveText(['authority_role']);

  await button.click();
  await expect.poll(async () => (await state(page)).updates).toBe(1);
  s = await state(page);
  expect(s.asked).toBe(1);
  expect(s.step.status).toBe('completed');
  expect(errors).toEqual([]);
});

test('a delivery without the shared ceremony file says the review cannot run, and offers no completion', async ({ page }) => {
  await mount(page, { metadata: { human_only: true }, withoutCeremony: true });
  await expect(page.locator('#host')).toContainText(
    'This review surface cannot run: /plugins/passkey-ceremony.js — it did not load',
  );
  await expect(page.getByRole('button', { name: COMPLETE, exact: true })).toHaveCount(0);
  expect((await state(page)).calls).toEqual([]);
});
