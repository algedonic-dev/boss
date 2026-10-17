import { expect, test, type Page } from './_test';
import { mountPage } from './_helpers';
import { installSmokeMocks, servePeopleRows } from './_smokeMocks';
import { readFile } from 'node:fs/promises';

const draft = { id: 'prepared-wire', user_id: 'emp-recipient', action: 'update', resource: 'control-example', scope: 'all', reason: 'Approved by a person', expires_at: '2026-12-01T10:30:00+00:00' };
async function open(page: Page) {
  await installSmokeMocks(page);
  const person = { id: 'emp-signed', name: 'Signed Human', email: 'signed@example.test', role: 'platform-admin', department: 'it', hire_date: '2026-01-01', status: 'active', location: 'HQ', employment_type: 'full-time', skills: [], certifications: [] };
  await page.route(/\/api\/session$/, r => r.fulfill({ json: { employee_id: person.id, username: 'signed' } }));
  await servePeopleRows(page, [person]);
  await page.route(/\/api\/policy\/rules$/, r => r.fulfill({ json: [] }));
  await mountPage(page, '/it/registry/policy', { titleMatch: /Policy rules/ });
  await expect(page.getByRole('heading', { name: 'Named-person permissions' })).toBeVisible();
}
async function prepare(page: Page, permission = draft) {
  if (!(await page.getByLabel('Prepared permission JSON').isVisible())) await page.getByText('Load a prepared permission', { exact: true }).click();
  await page.getByLabel('Prepared permission JSON').fill(JSON.stringify({ override: permission }));
  await page.getByRole('button', { name: 'Load prepared permission' }).click();
  await expect(page.getByLabel('Permission ID', { exact: true })).toHaveValue(permission.id);
  await page.getByRole('button', { name: 'Review permission' }).click();
  await expect(page.getByText('Review this permission before saving')).toBeVisible();
}

test('preparation never writes; explicit human save sends exact body and confirms GET', async ({ page }) => {
  const writes: unknown[] = [];
  const identities: Array<string | undefined> = [];
  let stored: Array<typeof draft> = [];
  const posted: string[] = [];
  page.on('request', request => { if (request.method() === 'POST' && request.url().endsWith('/api/policy/user-overrides')) posted.push(request.postData() ?? ''); });
  await open(page);
  expect(posted).toEqual([]);
  await page.route(/\/api\/policy\/user-overrides(?:\/[^/]+)?$/, async r => {
    if (r.request().method() === 'POST') {
      const body = r.request().postDataJSON() as { override: typeof draft };
      writes.push(body);
      identities.push(r.request().headers()['x-boss-user']);
      stored = [...stored, body.override];
      await r.fulfill({ json: body.override });
    } else await r.fulfill({ json: stored.map(row => ({ ...row, expires_at: '2026-12-01T10:30:00Z' })) });
  });
  expect(writes).toEqual([]);
  await prepare(page);
  expect(writes).toEqual([]);
  expect(posted).toEqual([]);
  await page.getByRole('button', { name: 'Save permission', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('Saved permission confirmed');
  expect(writes).toEqual([{ override: draft }]);
  expect(identities).toEqual([undefined]);
  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download permission receipt' }).click();
  expect((await download).suggestedFilename()).toBe('permission-prepared-wire.json');
  await page.getByRole('button', { name: 'Prepare another permission' }).click();
  const second = { ...draft, id: 'prepared-second', resource: 'another-control' };
  await prepare(page, second);
  expect(writes).toHaveLength(1);
  await page.getByRole('button', { name: 'Save permission', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('another-control');
  expect(writes).toEqual([{ override: draft }, { override: second }]);
});

test('backend refusal stays visible and never claims success', async ({ page }) => {
  await open(page);
  await page.route(/\/api\/policy\/user-overrides$/, r => r.fulfill({ status: 403, body: 'last human holder would be lost' }));
  await page.route(/\/api\/policy\/user-overrides\/[^/]+$/, r => r.fulfill({ json: [] }));
  await prepare(page);
  await page.getByRole('button', { name: 'Save permission', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('HTTP 403: last human holder would be lost');
  await expect(page.getByText('Saved permission confirmed')).toHaveCount(0);
});

test('different effect is uncertain and offers only readback, never a second blind write', async ({ page }) => {
  let writes = 0;
  let reads = 0;
  // The human recheck's GET is HELD until the test has seen the page while
  // it is in flight, and the count is judged only after its response and
  // the rendered result. A click resolves when it is dispatched, not when
  // the GET it starts is handled: counting straight after it read 2 on a
  // loaded runner (gate 75b09a85 at f98bb27b, green on the same head
  // before; backlog 16435a91), a test race, not a product one.
  let arrived: () => void = () => {};
  const recheckArrived = new Promise<void>(resolve => { arrived = resolve; });
  let release: () => void = () => {};
  const held = new Promise<void>(resolve => { release = resolve; });
  await open(page);
  await page.route(/\/api\/policy\/user-overrides(?:\/[^/]+)?$/, async r => {
    if (r.request().method() === 'POST') { writes++; await r.fulfill({ json: draft }); return; }
    reads++;
    if (reads === 3) { arrived(); await held; }
    await r.fulfill({ json: [{ ...draft, scope: 'none' }] });
  });
  await prepare(page);
  await page.getByRole('button', { name: 'Save permission', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('could not be confirmed exactly');
  await expect(page.getByRole('button', { name: 'Save permission', exact: true })).toBeDisabled();
  expect(writes).toBe(1);
  expect(reads).toBe(2);
  const recheck = page.waitForResponse(response => response.request().method() === 'GET' && /\/api\/policy\/user-overrides\/[^/]+$/.test(response.url()));
  await page.getByRole('button', { name: 'Check saved permission' }).click();
  await recheckArrived;
  // In flight: the explicit recheck is a GET, and nothing offers a write.
  await expect(page.getByRole('button', { name: 'Checking…' })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Check saved permission' })).toBeDisabled();
  expect(writes).toBe(1);
  release();
  await recheck;
  // Settled: still uncertain, still only the readback on offer.
  await expect(page.getByRole('button', { name: 'Check saved permission' })).toBeEnabled();
  await expect(page.getByRole('alert')).toContainText('could not be confirmed exactly');
  await expect(page.getByRole('button', { name: 'Save permission', exact: true })).toBeDisabled();
  expect(writes).toBe(1);
  expect(reads).toBe(3);
});

test('a failed pre-read never writes, and list labels keep the person actually queried', async ({ page }) => {
  let writes = 0;
  await open(page);
  await page.route(/\/api\/policy\/user-overrides(?:\/[^/]+)?$/, async r => {
    if (r.request().method() === 'POST') writes++;
    await r.fulfill({ json: [] });
  });
  await page.getByLabel('Person’s employee ID').fill('emp-original');
  await page.getByRole('button', { name: 'View current permissions' }).click();
  await expect(page.getByRole('heading', { name: 'Current permissions for emp-original' })).toBeVisible();
  await page.getByLabel('Person’s employee ID').fill('emp-other');
  await expect(page.getByRole('heading', { name: 'Current permissions for emp-original' })).toBeVisible();
  await prepare(page);
  await page.route(/\/api\/policy\/user-overrides\/[^/]+$/, r => r.fulfill({ status: 503, body: 'roster unavailable' }));
  await page.getByRole('button', { name: 'Save permission', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('No permission was submitted');
  expect(writes).toBe(0);
});

test('a final download retains the first mismatch, failed GET recheck and later exact response', async ({ page }) => {
  await open(page);
  let reads = 0;
  let writes = 0;
  const mismatch = JSON.stringify([{ ...draft, scope: 'none' }]);
  const exact = JSON.stringify([draft]);
  await page.route(/\/api\/policy\/user-overrides(?:\/[^/]+)?$/, async route => {
    if (route.request().method() === 'POST') { writes++; await route.fulfill({ json: draft }); return; }
    reads++;
    if (reads === 3) { await route.abort('failed'); return; }
    await route.fulfill({ contentType: 'application/json', body: reads === 1 ? '[]' : reads === 2 ? mismatch : exact });
  });
  await prepare(page);
  await page.getByRole('button', { name: 'Save permission', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('could not be confirmed exactly');
  await page.getByRole('button', { name: 'Check saved permission' }).click();
  await expect(page.getByRole('alert')).toContainText('could not be confirmed:');
  await expect(page.getByRole('button', { name: 'Save permission', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: 'Check saved permission' }).click();
  await expect(page.getByRole('status')).toContainText('Saved permission confirmed');
  const downloading = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Download permission receipt' }).click();
  const path = await (await downloading).path();
  if (!path) throw new Error('No downloaded receipt path');
  const receipt = JSON.parse(await readFile(path, 'utf8')) as {
    after: { body: string }; current_after: { body: string };
    readbacks: Array<{ response: { body: string } | null; error: string | null; status: number | null }>;
  };
  expect(receipt.after.body).toBe(mismatch);
  expect(receipt.current_after.body).toBe(exact);
  expect(receipt.readbacks.map(read => read.response?.body ?? null)).toEqual([mismatch, null, exact]);
  expect(receipt.readbacks.map(read => read.status)).toEqual([200, null, 200]);
  expect(receipt.readbacks[1]?.error).toBeTruthy();
  expect(writes).toBe(1);
});
