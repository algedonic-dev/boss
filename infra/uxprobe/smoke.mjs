// Post-converge smoke: the standard sweep behind ship-a-change's
// `proven` step (phase 1 of the proven-in-prod protocol — the agent
// runs this after every converge and records its findings as the
// step's `verified` evidence; phase 2 wires it up as the automated
// completer).
//
// Deliberately a fixed sweep, not a per-car script: the per-car check
// is the operator's judgment call written into `verified`; this is
// the floor under it — the surfaces every deploy must not have
// broken, plus the console/network error capture that catches the
// class of "renders fine, dies quietly".
//
// Run (see README.md for the tunnel + guest-session mechanics):
//   BASE=http://127.0.0.1:18080 node smoke.mjs
// Exit code: 0 all checks pass, 1 any check fails — so a runner can
// gate on it without parsing the log.
import { chromium } from 'playwright';

const BASE = process.env.BASE ?? 'http://127.0.0.1:18080';
const here = new URL('.', import.meta.url).pathname;
const out = (n) => `${here}smoke-${n}.png`;

const results = [];
const check = (name, ok, detail = '') => {
  results.push({ name, ok, detail });
  console.log(`[smoke] ${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` — ${detail}` : ''}`);
};

const mint = await fetch(BASE + '/api/auth/guest', { method: 'POST' });
const cookieVal = (mint.headers.get('set-cookie') ?? '').match(/boss_session=([^;]+)/)?.[1];
check('guest session mints', Boolean(cookieVal), `HTTP ${mint.status}`);
if (!cookieVal) process.exit(1);

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
await ctx.addCookies([
  { name: 'boss_session', value: cookieVal, domain: new URL(BASE).hostname, path: '/', secure: false },
]);
const page = await ctx.newPage();

const pageErrors = [];
page.on('pageerror', (e) => pageErrors.push(String(e).slice(0, 200)));
const badResponses = [];
page.on('response', (r) => {
  // 401/403 are policy answers a guest legitimately collects on some
  // surfaces; 5xx and 404 on same-origin API calls are deploy damage.
  if (r.status() >= 500 || r.status() === 404) {
    const u = new URL(r.url());
    if (u.pathname.startsWith('/api/') || u.pathname.endsWith('.js')) {
      badResponses.push(`${r.status()} ${r.request().method()} ${u.pathname}`);
    }
  }
});

// 1. The shell mounts. If the SPA bundle is broken the app dies before
//    any route renders — the class of failure a svelte-window lookup
//    once caused, invisible to every server-side check.
await page.goto(BASE + '/', { waitUntil: 'networkidle', timeout: 30000 });
await page.waitForTimeout(1500);
await page.screenshot({ path: out('home') });
const homeText = await page.locator('body').innerText();
check('shell mounts with navigation', /MY DAY/i.test(homeText) && /ALL JOBS/i.test(homeText));
check('home renders content below the chrome', homeText.length > 400, `${homeText.length} chars`);

// 2. A jobs list renders rows from the API — the read path end to end.
await page.goto(BASE + '/ux/jobs', { waitUntil: 'networkidle', timeout: 30000 });
await page.waitForTimeout(1500);
await page.screenshot({ path: out('jobs') });
const jobsText = await page.locator('body').innerText();
check('jobs list renders', jobsText.length > 300, `${jobsText.length} chars`);

// 3. A step surface renders with its decision panel. The probe target
//    is any open packet's first open step, discovered live — a fixed
//    id would rot the day its packet closed.
const jobsResp = await page.request.get(BASE + '/api/jobs?kind=user-feedback&status=open&limit=5');
const open = jobsResp.ok() ? (await jobsResp.json()).data ?? [] : [];
const target = open
  .flatMap((j) =>
    (j.steps ?? [])
      .filter((s) => s.status === 'ready' || s.status === 'active')
      .map((s) => ({ job: j.id, step: s.id })),
  )
  .at(0);
if (target) {
  await page.goto(`${BASE}/ux/jobs/${target.job}/steps/${target.step}`, {
    waitUntil: 'load',
    timeout: 30000,
  });
  await page.waitForTimeout(2500);
  await page.screenshot({ path: out('step-focus'), fullPage: true });
  const stepText = await page.locator('body').innerText();
  check('step focus renders a surface', stepText.length > 200, `${stepText.length} chars`);
  // Raw markdown leaking = the renderer chain broke somewhere.
  check(
    'briefs render as markdown, not raw syntax',
    !/^##\s|\*\*[A-Za-z]/m.test(stepText),
  );
} else {
  check('step focus renders a surface', true, 'skipped — no open packet to probe');
}

// 4. Nothing died quietly.
check('no page errors', pageErrors.length === 0, pageErrors.join(' | ').slice(0, 300));
check('no 5xx/404 on API or bundles', badResponses.length === 0, [...new Set(badResponses)].join(' | ').slice(0, 300));

await browser.close();
const failed = results.filter((r) => !r.ok);
console.log(`[smoke] ${results.length - failed.length}/${results.length} checks pass`);
process.exit(failed.length ? 1 : 0);
