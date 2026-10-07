import { readFileSync } from 'node:fs';
import { expect, test, type Page } from './_test';

// Drive the actual standalone bundles through the host contract. The
// in-memory server keeps a successful merge even if completion refuses:
// two doors are two writes, not a transaction (backlog c9e0d8b2).
const cases = [
  {kind: 'sr-triage', save: 'Save draft', done: 'Complete triage', metadata: {
    account_id: 'acc', device_serial: 'serial', failure_description: 'fault',
    priority: 'urgent', triage_outcome: 'dispatch', foreign: 'keep',
  }},
  {kind: 'diagnostic-call', save: 'Save draft', done: 'Close call', metadata: {
    outcome: 'diagnosed', foreign: 'keep',
  }},
  {kind: 'checklist', save: 'Save', done: 'All checked — complete step', metadata: {
    items: [{label: 'Read the record', checked: true}], foreign: 'keep',
  }},
] as const;

type State = {calls: {method: string; body: Record<string, unknown>}[]; updates: number;
  row: {status: string; assignee_id: string; metadata: Record<string, unknown>}};
async function mount(page: Page, entry: typeof cases[number], failure: 'PATCH' | 'PUT' | 'network' | 'PUT-network' | 'PATCH-network-committed' | 'PUT-race' | null) {
  const errors: string[] = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  await page.setContent('<div id="host"></div>');
  await page.evaluate(({entry, failure}) => {
    const row = {id: 'step', kind: entry.kind, title: 'Work', status: 'pending',
      assignee_id: 'emp-owner', metadata: {...entry.metadata} as Record<string, unknown>};
    const state: State = {calls: [], updates: 0, row};
    const w = window as unknown as Record<string, unknown>;
    w['__saveState'] = state;
    w['__retryOK'] = false;
    w['__boss_register_step_plugin'] = (_kind: string, fn: unknown) => {w['__mount'] = fn;};
    w['fetch'] = async (url: string, options: RequestInit) => {
      const method = options.method ?? 'GET';
      const body = JSON.parse(String(options.body)) as Record<string, unknown>;
      state.calls.push({method, body});
      if (!w['__retryOK'] && failure === 'network') throw new Error('connection lost');
      if (!w['__retryOK'] && failure === 'PATCH-network-committed' && method === 'PATCH') {
        row.metadata = {...row.metadata, ...body};
        throw new Error('save response lost');
      }
      if (!w['__retryOK'] && failure === 'PUT-race' && method === 'PUT') {
        row.status = 'completed'; // another actor completed before our rejected request
        return new Response('STEP_CHANGED: already completed', {status: 409});
      }
      if (!w['__retryOK'] && failure === 'PUT-network' && method === 'PUT') {
        row.status = String(body.status); // server committed; the response disappeared
        throw new Error('response lost');
      }
      if (!w['__retryOK'] && failure === method) {
        return new Response('refused <img src=x onerror=alert(1)>', {status: method === 'PATCH' ? 403 : 409});
      }
      if (method === 'PATCH' && url.endsWith('/metadata')) row.metadata = {...row.metadata, ...body};
      else if (method === 'PUT' && url.endsWith('/step')) row.status = String(body.status);
      else throw new Error('unexpected door: ' + method + ' ' + url);
      return new Response(null, {status: 204});
    };
  }, {entry, failure});
  await page.addScriptTag({content: readFileSync(new URL(`../../../../infra/step-plugins/${entry.kind}.js`, import.meta.url), 'utf8')});
  await page.evaluate(() => {
    const w = window as unknown as Record<string, unknown>;
    const state = w['__saveState'] as State;
    const fn = w['__mount'] as (host: Element, args: unknown) => void;
    fn(document.getElementById('host')!, {step: structuredClone(state.row), jobId: 'job',
      onUpdate() {state.updates++;}});
  });
  return errors;
}

test('diagnostic-call: a refused Waive shows its phase; retry skips without inventing an end time', async ({page}) => {
  const errors = await mount(page, cases[1], 'PUT');
  const waive = page.getByRole('button', {name: 'Waive call', exact: true});
  await waive.click();
  await expect(page.getByRole('alert')).toContainText('Skip request refused. Fields were saved');
  const before = await state(page);
  expect(before.updates).toBe(0);
  expect(before.row.status).toBe('pending');
  expect(before.row.metadata).not.toHaveProperty('ended_at');
  expect(before.calls[1]!.body).toEqual({status: 'skipped'});
  await page.evaluate(() => {(window as unknown as {__retryOK: boolean}).__retryOK = true;});
  await waive.click();
  await expect.poll(async () => (await state(page)).updates).toBe(1);
  expect((await state(page)).row.status).toBe('skipped');
  expect((await state(page)).row.metadata).not.toHaveProperty('ended_at');
  expect(errors).toEqual([]);
});
const state = (page: Page) => page.evaluate(() => (window as unknown as {__saveState: State}).__saveState);

for (const entry of cases) {
  test(`${entry.kind}: a draft preserves status, holder and foreign metadata`, async ({page}) => {
    const errors = await mount(page, entry, null);
    await page.getByRole('button', {name: entry.save, exact: true}).click();
    await expect.poll(async () => (await state(page)).updates).toBe(1);
    const s = await state(page);
    expect(s.calls.map((c) => c.method)).toEqual(['PATCH']);
    expect(s.calls[0]!.body).not.toHaveProperty('status');
    expect(s.calls[0]!.body).not.toHaveProperty('assignee_id');
    expect(s.row.status).toBe('pending');
    expect(s.row.assignee_id).toBe('emp-owner');
    expect(s.row.metadata.foreign).toBe('keep');
    expect(errors).toEqual([]);
  });
  for (const failure of ['PATCH', 'PUT', 'network'] as const) {
    test(`${entry.kind}: ${failure} refusal is visible, no success refresh, retry recovers`, async ({page}) => {
      const errors = await mount(page, entry, failure);
      const button = page.getByRole('button', {name: entry.done, exact: true});
      await button.click();
      const alert = page.getByRole('alert');
      await expect(alert).toBeVisible();
      await expect(alert).toContainText(failure === 'PUT' ? 'Completion request refused' : failure === 'network' ? 'Save not confirmed' : 'Save request refused');
      await expect(alert).toContainText(failure === 'network' ? 'connection lost' : 'refused <img');
      await expect(page.locator('#host img')).toHaveCount(0);
      const s = await state(page);
      expect(s.updates).toBe(0);
      expect(s.row.status).toBe('pending');
      expect(s.row.assignee_id).toBe('emp-owner');
      expect(s.row.metadata.foreign).toBe('keep');
      expect(s.calls.map((c) => c.method)).toEqual(failure === 'PUT' ? ['PATCH', 'PUT'] : ['PATCH']);
      if (entry.kind === 'diagnostic-call' && failure === 'PUT') {
        expect(s.row.metadata.ended_at).toEqual(expect.any(String));
        await expect(alert).toContainText('Fields were saved');
      }
      await expect(button).toBeEnabled();
      await page.evaluate(() => {(window as unknown as {__retryOK: boolean}).__retryOK = true;});
      await button.click();
      await expect.poll(async () => (await state(page)).updates).toBe(1);
      await expect(alert).toBeHidden();
      expect((await state(page)).row.status).toBe('completed');
      if (entry.kind === 'diagnostic-call' && failure === 'PUT') {
        expect((await state(page)).row.metadata.ended_at).toBe(s.row.metadata.ended_at);
      }
      expect(errors).toEqual([]);
    });
  }
  test(`${entry.kind}: successful completion merges first then sends status alone`, async ({page}) => {
    const errors = await mount(page, entry, null);
    await page.getByRole('button', {name: entry.done, exact: true}).click();
    await expect.poll(async () => (await state(page)).updates).toBe(1);
    const s = await state(page);
    expect(s.calls.map((c) => c.method)).toEqual(['PATCH', 'PUT']);
    expect(s.calls[1]!.body).toEqual({status: 'completed'});
    expect(s.row.metadata.foreign).toBe('keep');
    expect(s.row.assignee_id).toBe('emp-owner');
    expect(s.row.status).toBe('completed');
    expect(errors).toEqual([]);
  });
  test(`${entry.kind}: a lost completion response is unknown, never a claimed refusal`, async ({page}) => {
    const errors = await mount(page, entry, 'PUT-network');
    await page.getByRole('button', {name: entry.done, exact: true}).click();
    await expect(page.getByRole('alert')).toContainText('Completion not confirmed');
    await expect(page.getByRole('alert')).toContainText('response lost');
    expect((await state(page)).updates).toBe(0);
    expect((await state(page)).row.status).toBe('completed');
    expect(errors).toEqual([]);
  });
  test(`${entry.kind}: a lost field-save reply remains unknown even if the merge persisted`, async ({page}) => {
    const errors = await mount(page, entry, 'PATCH-network-committed');
    await page.getByRole('button', {name: entry.done, exact: true}).click();
    await expect(page.getByRole('alert')).toContainText('Save not confirmed');
    await expect(page.getByRole('alert')).toContainText('save response lost');
    const s = await state(page);
    expect(s.calls.map((c) => c.method)).toEqual(['PATCH']);
    expect(s.row.status).toBe('pending');
    expect(s.updates).toBe(0);
    if (entry.kind === 'diagnostic-call') {
      expect(s.row.metadata.ended_at).toEqual(expect.any(String));
      await page.evaluate(() => {
        (window as unknown as Record<string, unknown>)['__retryOK'] = true;
      });
      await page.getByRole('button', {name: entry.done, exact: true}).click();
      await expect.poll(async () => (await state(page)).updates).toBe(1);
      expect((await state(page)).row.metadata.ended_at).toBe(s.row.metadata.ended_at);
      expect((await state(page)).row.status).toBe('completed');
    }
    expect(errors).toEqual([]);
  });
  test(`${entry.kind}: completion refusal describes our request, not the global step status`, async ({page}) => {
    const errors = await mount(page, entry, 'PUT-race');
    await page.getByRole('button', {name: entry.done, exact: true}).click();
    const alert = page.getByRole('alert');
    await expect(alert).toContainText('Completion request refused');
    await expect(alert).toContainText('STEP_CHANGED');
    await expect(alert).not.toContainText('Step not completed');
    expect((await state(page)).row.status).toBe('completed');
    expect((await state(page)).updates).toBe(0);
    expect(errors).toEqual([]);
  });
}
