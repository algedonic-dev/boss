import { expect, test } from './_test';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { installApiFloor } from './_smokeMocks';

const source = () => readFileSync(new URL('../../../../infra/step-plugins/browser-observation.js', import.meta.url), 'utf8');
// The contract's pinned renderer, not the served tree file: whether the served
// file still matches the pin is one named Rust check
// (the_served_renderer_and_suite_are_the_bytes_the_browser_contract_pins), so a
// drift does not also fail this spec (review 6ac43de2 F1, 2026-10-20).
const pinnedRenderer = () => readFileSync(new URL('../../../../crates/core/boss-jobs/tests/fixtures/browser-renderer/answer-question.js', import.meta.url));

for (const fault of ['none', 'wrong-resource', 'attachment-bytes', 'runtime-error'] as const) {
test(`explicit submission conserves originals and refuses ${fault}`, async ({ page }) => {
  await installApiFloor(page);
  const contract = JSON.parse(readFileSync(new URL('../../../../infra/step-plugins/browser-renderer-contract.json', import.meta.url), 'utf8')) as { car: string; source_head: string; asset: { bytes: number; sha256: string } };
  const renderer = fault === 'runtime-error' ? Buffer.from('// renderer registration missing') : pinnedRenderer();
  if (fault === 'runtime-error') contract.asset = { ...contract.asset, bytes: renderer.length, sha256: createHash('sha256').update(renderer).digest('hex') };
  const suite = readFileSync(new URL('../../../../infra/step-plugins/browser-renderer-cases.js', import.meta.url));
  const files = new Map<string, Buffer>();
  const writes: { method: string; url: string; body: unknown }[] = [];
  let original: unknown;
  await page.route('**/plugins/browser-renderer-contract.json', (route) => route.fulfill({ json: { ...contract, origin: new URL(route.request().url()).origin } }));
  await page.route('**/plugins/answer-question.js', (route) => route.fulfill({ body: renderer }));
  await page.route('**/plugins/browser-renderer-cases.js', (route) => route.fulfill({ body: suite }));
  await page.route('**/api/jobs/observation-job', (route) => route.fulfill({ json: { kind: 'browser-renderer-observation', metadata: { car: contract.car } } }));
  await page.route('**/api/jobs/' + contract.car, (route) => route.fulfill({ json: { id: contract.car, kind: 'ship-a-change', steps: [{ spec_slug: 'gate', metadata: { receipt: { head: contract.source_head, verdict: 'green' } } }] } }));
  await page.route('**/api/files', async (route) => {
    const request = route.request();
    const form = await new Request(request.url(), { method: 'POST', headers: request.headers(), body: Uint8Array.from(request.postDataBuffer() || []).buffer }).formData();
    const file = form.get('file');
    if (!(file instanceof File)) throw new Error('uploaded file missing');
    const bytes = Buffer.from(await file.arrayBuffer());
    const id = `00000000-0000-4000-8000-${String(files.size + 1).padStart(12, '0')}`;
    files.set(id, bytes);
    await route.fulfill({ status: 201, json: { id, target: { kind: form.get('target_kind'), id: form.get('target_id') }, size_bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') } });
  });
  await page.route('**/api/files/*', (route) => route.fulfill({ body: fault === 'attachment-bytes' ? Buffer.from('tampered') : files.get(route.request().url().split('/').pop() || '') || Buffer.alloc(0) }));
  await page.route('**/api/jobs/observation-job/steps/observation-step/metadata/records', async (route) => {
    const body = route.request().postDataJSON() as { value: unknown; expected_absence: boolean; key: string };
    writes.push({ method: route.request().method(), url: route.request().url(), body });
    original = { job_id: fault === 'wrong-resource' ? 'another-job' : 'observation-job', step_id: 'observation-step', key: body.key, value: body.value, actor: 'emp-observer', recorded_at: '2026-10-15T06:00:00Z', event_id: '00000000-0000-4000-8000-000000000099' };
    await route.fulfill({ status: 201, json: { result: 'recorded', record: original } });
  });
  await page.route('**/api/jobs/observation-job/steps/observation-step/records/observation', (route) => route.fulfill({ json: original }));
  // The observation is the signer's (rework of car b16e373d): the same
  // signed-in session stamps the step's required role after the record.
  await page.route('**/api/jobs/observation-job/steps/observation-step/sign-offs', async (route) => {
    writes.push({ method: route.request().method(), url: route.request().url(), body: route.request().postDataJSON() });
    await route.fulfill({ json: { id: 'observation-step' } });
  });
  await page.route('**/api/jobs/observation-job/steps/observation-step', async (route) => {
    writes.push({ method: route.request().method(), url: route.request().url(), body: route.request().postDataJSON() });
    await route.fulfill({ status: 204 });
  });
  await page.goto('/');
  await page.evaluate(() => { (window as unknown as { __boss_register_step_plugin: (_kind: string, mount: unknown) => void; __mount: unknown }).__boss_register_step_plugin = (_kind, mount) => { (window as unknown as { __mount: unknown }).__mount = mount; }; });
  await page.addScriptTag({ content: source() });
  await page.evaluate(() => {
    const host = document.createElement('div'); document.body.replaceChildren(host);
    (window as unknown as { __mount: (host: Element, props: unknown) => void }).__mount(host, { jobId: 'observation-job', step: { id: 'observation-step', status: 'ready', sign_offs_required: ['platform-admin'] }, onUpdate: () => {} });
  });
  await page.getByRole('button', { name: 'Run observations' }).click();
  if (fault === 'runtime-error') {
    await page.getByRole('button', { name: 'Submit incomplete evidence' }).click();
    await expect(page.getByText('Incomplete evidence retained. A new observation is required; qualification remains refused.')).toBeVisible();
    expect(files.size).toBe(3);
    expect(writes.map(({ method }) => method)).toEqual(['POST']);
    return;
  }
  await expect(page.getByRole('button', { name: 'Submit complete evidence' })).toBeVisible();
  expect(writes).toEqual([]);
  expect(files.size).toBe(0);
  await page.getByRole('button', { name: 'Submit complete evidence' }).click();
  if (fault !== 'none') {
    await expect(page.getByText(fault === 'wrong-resource' ? /immutable observation did not read back whole/ : /attachment/i).last()).toBeVisible();
    expect(writes.filter(({ method }) => method === 'PUT')).toEqual([]);
    return;
  }
  await expect(page.getByText('Evidence recorded. Independent qualification remains required. No car proof was completed.')).toBeVisible();
  expect(files.size).toBe(3);
  expect([...files.values()][0]).toEqual(renderer);
  expect([...files.values()][1]).toEqual(suite);
  expect(writes.map(({ method, body }) => ({ method, body }))).toEqual([
    { method: 'POST', body: expect.objectContaining({ key: 'observation', expected_absence: true }) },
    { method: 'POST', body: { role: 'platform-admin' } },
    { method: 'PUT', body: { status: 'completed' } },
  ]);
});
}

test('asset controls keep genuine bytes and refuse redirects, wrong identity, oversize and malformed UTF-8', async ({ page }) => {
  await installApiFloor(page);
  await page.goto('/');
  await page.addScriptTag({ content: source() });
  const results = await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { digest: (bytes: Uint8Array) => Promise<string>; asset: (binding: unknown, response: Response) => Promise<unknown> } }).__boss_browser_observation;
    const good = new TextEncoder().encode('synthetic');
    const hash = await api.digest(good);
    const checks = [];
    for (const fault of ['none', 'redirect', 'status', 'url', 'origin', 'digest', 'size', 'oversize', 'utf8']) {
      const bytes = fault === 'oversize' ? new Uint8Array(262145) : fault === 'utf8' ? new Uint8Array([255]) : good;
      const binding = { path: '/plugins/synthetic.js', sha256: fault === 'digest' ? '0'.repeat(64) : await api.digest(bytes), bytes: fault === 'size' ? 1 : Math.min(bytes.length, 262144) };
      const response = new Response(bytes, { status: fault === 'status' ? 201 : 200 });
      Object.defineProperty(response, 'url', { value: fault === 'origin' ? 'https://foreign.invalid/plugins/synthetic.js' : location.origin + (fault === 'url' ? '/plugins/other.js' : binding.path) });
      Object.defineProperty(response, 'redirected', { value: fault === 'redirect' });
      const accepted = await api.asset(binding, response).then(() => true, () => false);
      checks.push({ fault, accepted });
    }
    return { checks, hash };
  });
  expect(results.hash).toHaveLength(64);
  expect(results.checks).toEqual(['none', 'redirect', 'status', 'url', 'origin', 'digest', 'size', 'oversize', 'utf8'].map((fault) => ({ fault, accepted: fault === 'none' })));
});

test('missing cases and forged parent messages never become a complete run', async ({ page }) => {
  await installApiFloor(page);
  await page.goto('/');
  await page.addScriptTag({ content: source() });
  const results = await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { output: (value: unknown, ids: string[]) => unknown; run: (host: Element, renderer: unknown, suite: unknown, ids: string[], timeout: number) => Promise<unknown> } }).__boss_browser_observation;
    let missingRefused = false;
    try { api.output({ schema: 1, passed: true, cases: [] }, ['required']); } catch { missingRefused = true; }
    const host = document.createElement('div'); document.body.replaceChildren(host);
    const bytes = { bytes: new TextEncoder().encode('') };
    const stalled = { bytes: new TextEncoder().encode('window.__boss_renderer_cases={run:()=>new Promise(()=>{})};') };
    const promise = api.run(host, bytes, stalled, ['required'], 100);
    window.postMessage({ nonce: 'forged', result: { schema: 1, cases: [] } }, '*');
    const wrapper = host.querySelector('iframe');
    const encoded = wrapper?.srcdoc.match(/base64,([^\"]+)/)?.[1];
    if (!encoded) throw new Error('trusted relay source missing');
    const match = atob(encoded).match(/event.data.nonce !== ("[^"]+")/);
    if (!match) throw new Error('relay nonce binding missing');
    const nonce: unknown = JSON.parse(match[1] || 'null');
    // Even the genuine nonce from the wrong window is not a runner reply.
    window.postMessage({ nonce, result: { schema: 1, cases: [{ id: 'required', assertions: [{ name: 'forged', passed: true, actual: 'forged' }], requests: [], error: null }] } }, '*');
    const timedOut = await promise.then(() => false, (error: Error) => error.message.includes('timed out'));
    const failed = await api.run(host, bytes, bytes, ['required'], 1000).then(() => false, (error: Error) => error.message.includes('runner failed'));
    return { missingRefused, timedOut, failed, frames: host.querySelectorAll('iframe').length };
  });
  expect(results).toEqual({ missingRefused: true, timedOut: true, failed: true, frames: 0 });
});

test('the opaque frame cannot send a forbidden network request', async ({ page }) => {
  await installApiFloor(page);
  let requests = 0;
  await page.route('https://forbidden.invalid/**', (route) => { requests++; return route.abort(); });
  await page.goto('/');
  await page.addScriptTag({ content: source() });
  const result = await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { run: (host: Element, renderer: unknown, suite: unknown, ids: string[]) => Promise<{ cases: { assertions: { passed: boolean }[] }[] }> } }).__boss_browser_observation;
    const suite = `window.__boss_renderer_cases={run:async()=>{let blocked=false;try{await fetch('https://forbidden.invalid/no-live-write',{method:'POST',body:'synthetic'});}catch{blocked=true;}return {schema:1,cases:[{id:'network',assertions:[{name:'network blocked',passed:blocked,actual:String(blocked)}],requests:[],error:null}]};}};`;
    const bytes = (value: string) => ({ bytes: new TextEncoder().encode(value) });
    const host = document.createElement('div'); document.body.replaceChildren(host);
    return api.run(host, bytes(''), bytes(suite), ['network']);
  });
  expect(result.cases[0]?.assertions[0]?.passed).toBe(true);
  expect(requests).toBe(0);
});

test('the opaque frame cannot navigate to a network destination', async ({ page }) => {
  await installApiFloor(page);
  let requests = 0;
  await page.route('https://forbidden.invalid/**', (route) => { requests++; return route.abort(); });
  await page.goto('/');
  await page.addScriptTag({ content: source() });
  await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { run: (host: Element, renderer: unknown, suite: unknown, ids: string[], timeout: number) => Promise<unknown> } }).__boss_browser_observation;
    const suite = `window.__boss_renderer_cases={run:async()=>{location.href='https://forbidden.invalid/navigation';return new Promise(()=>{});}};`;
    const bytes = (value: string) => ({ bytes: new TextEncoder().encode(value) });
    const host = document.createElement('div'); document.body.replaceChildren(host);
    await api.run(host, bytes(''), bytes(suite), ['navigation'], 500).catch(() => {});
  });
  expect(requests).toBe(0);
});

test('unsupported wrapper policy refuses before any renderer executes', async ({ page }) => {
  await installApiFloor(page);
  await page.goto('/');
  await page.addScriptTag({ content: source().replace('frame-src about:', 'frame-src about: data: https: http:') });
  const refused = await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { run: (host: Element, renderer: unknown, suite: unknown, ids: string[], timeout: number) => Promise<unknown> } }).__boss_browser_observation;
    const suite = `window.__boss_renderer_cases={run:async()=>({schema:1,cases:[{id:'probe',assertions:[{name:'executed',passed:true,actual:'yes'}],requests:[],error:null}]})};`;
    const bytes = (value: string) => ({ bytes: new TextEncoder().encode(value) });
    const host = document.createElement('div'); document.body.replaceChildren(host);
    try { await api.run(host, bytes(''), bytes(suite), ['probe'], 1500); return false; } catch { return true; }
  });
  expect(refused).toBe(true);
});

test('inner scripts cannot navigate ancestors, open windows, submit forms or load resources', async ({ page }) => {
  await installApiFloor(page);
  let requests = 0;
  await page.route('**/blocked-isolation-control/**', (route) => { requests++; return route.abort(); });
  await page.goto('/');
  const before = page.url();
  const popups: unknown[] = [];
  page.on('popup', (popup) => { popups.push(popup); });
  await page.addScriptTag({ content: source() });
  const results = await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { run: (host: Element, renderer: unknown, suite: unknown, ids: string[], timeout: number) => Promise<unknown> } }).__boss_browser_observation;
    const attempts = [
      "location.href='http://forbidden.invalid/blocked-isolation-control/http'",
      "location.replace('https://forbidden.invalid/blocked-isolation-control/redirect')",
      `location.href=${JSON.stringify(location.origin + '/blocked-isolation-control/same-origin')}`,
      "parent.location='https://forbidden.invalid/blocked-isolation-control/parent'",
      "top.location='https://forbidden.invalid/blocked-isolation-control/top'",
      "window.open('https://forbidden.invalid/blocked-isolation-control/popup')",
      "const form=document.createElement('form');form.action='https://forbidden.invalid/blocked-isolation-control/form';document.body.append(form);form.submit()",
      "const image=new Image();image.src='https://forbidden.invalid/blocked-isolation-control/image';document.body.append(image)",
      "const script=document.createElement('script');script.src='https://forbidden.invalid/blocked-isolation-control/script';document.body.append(script)",
      "const frame=document.createElement('iframe');frame.src='https://forbidden.invalid/blocked-isolation-control/frame';document.body.append(frame)",
    ];
    const bytes = (value: string) => ({ bytes: new TextEncoder().encode(value) });
    const results = [];
    for (const attempt of attempts) {
      const suite = `window.__boss_renderer_cases={run:async()=>{try{${attempt}}catch{}await new Promise(resolve=>setTimeout(resolve,40));return {schema:1,cases:[{id:'isolation',assertions:[{name:'attempt retained',passed:true,actual:'attempted'}],requests:[],error:null}]};}};`;
      const host = document.createElement('div'); document.body.replaceChildren(host);
      try { await api.run(host, bytes(''), bytes(suite), ['isolation'], 2000); results.push(true); } catch { results.push(false); }
    }
    return results;
  });
  // A refused document navigation can discard the child document. Its
  // missing reply must remain incomplete; the other blocked operations
  // leave the synthetic document alive to return its full report.
  expect(results).toEqual([false, false, false, true, true, true, true, true, true, true]);
  expect(requests).toBe(0);
  expect(popups).toEqual([]);
  expect(page.url()).toBe(before);
});

test('the observation surface resolves its car from the published binding and needs an explicit start', async ({ page }) => {
  await installApiFloor(page);
  const contract = JSON.parse(readFileSync(new URL('../../../../infra/step-plugins/browser-renderer-contract.json', import.meta.url), 'utf8')) as { car: string; source_head: string };
  await page.route('**/plugins/browser-renderer-contract.json', (route) => route.fulfill({ json: { ...contract, origin: new URL(route.request().url()).origin } }));
  await page.route('**/api/jobs/observation-job', (route) => route.fulfill({ json: { kind: 'browser-renderer-observation', metadata: { car: contract.car } } }));
  await page.route('**/api/jobs/' + contract.car, (route) => route.fulfill({ json: { id: contract.car, kind: 'ship-a-change', steps: [{ spec_slug: 'gate', metadata: { receipt: { head: contract.source_head, verdict: 'green' } } }] } }));
  await page.goto('/');
  await page.evaluate(() => { (window as unknown as { __boss_register_step_plugin: (kind: string, mount: unknown) => void; __observation_mount: unknown }).__boss_register_step_plugin = (_kind, mount) => { (window as unknown as { __observation_mount: unknown }).__observation_mount = mount; }; });
  await page.addScriptTag({ content: source() });
  await page.evaluate(() => {
    const host = document.createElement('div'); document.body.replaceChildren(host);
    (window as unknown as { __observation_mount: (host: Element, props: unknown) => void }).__observation_mount(host, { jobId: 'observation-job', step: { id: 'observation-step', status: 'ready' }, onUpdate: () => {} });
  });
  await expect(page.getByRole('button', { name: 'Run observations' })).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Submit complete evidence' })).toHaveCount(0);
  await expect(page.getByText('Attended browser evidence. Independent qualification is still required.')).toBeVisible();
});

test('the same published synthetic suite runs exact renderer bytes inside the opaque sandbox', async ({ page }) => {
  await installApiFloor(page);
  await page.goto('/');
  await page.addScriptTag({ content: source() });
  const renderer = pinnedRenderer().toString('utf8');
  const suite = readFileSync(new URL('../../../../infra/step-plugins/browser-renderer-cases.js', import.meta.url), 'utf8');
  const contract = JSON.parse(readFileSync(new URL('../../../../infra/step-plugins/browser-renderer-contract.json', import.meta.url), 'utf8')) as { assertions: Record<string, string[]> };
  const result = await page.evaluate(async ({ renderer, suite }) => {
    const api = (window as unknown as { __boss_browser_observation: { run: (host: Element, renderer: unknown, suite: unknown, ids: string[]) => Promise<{ cases: { id: string; assertions: { passed: boolean }[]; error: string | null }[] }> } }).__boss_browser_observation;
    const host = document.createElement('div'); document.body.replaceChildren(host);
    const bytes = (value: string) => ({ bytes: new TextEncoder().encode(value) });
    return api.run(host, bytes(renderer), bytes(suite), ['question', 'decision', 'contradiction', 'retry', 'completed', 'missing']);
  }, { renderer, suite });
  expect(result.cases).toHaveLength(6);
  for (const row of result.cases) {
    expect(row.error, row.id).toBeNull();
    expect(row.assertions.every((assertion) => assertion.passed), row.id).toBe(true);
    expect(row.assertions.map((assertion) => (assertion as { name?: string }).name), row.id).toEqual(contract.assertions[row.id]);
  }
});

test('the observation runner refuses unbound assets before executing any script', async ({ page }) => {
  await installApiFloor(page);
  await page.goto('/');
  await page.addScriptTag({ content: source() });
  const refused = await page.evaluate(async () => {
    const api = (window as unknown as { __boss_browser_observation: { asset: (binding: unknown, response: Response) => Promise<unknown> } }).__boss_browser_observation;
    return api.asset({ path: '/plugins/answer-question.js', sha256: '0'.repeat(64), bytes: 3 }, new Response('bad'))
      .then(() => false, () => true);
  });
  expect(refused).toBe(true);
});
