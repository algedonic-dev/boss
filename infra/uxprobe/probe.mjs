// Browser-truth probe: render a step surface on the live gateway in
// real Chromium and report what an operator actually gets. See
// README.md for why this exists and how to run it.
import { chromium } from 'playwright';

const BASE = process.env.BASE ?? 'http://127.0.0.1:18080';
const JOB = process.env.JOB;
const STEP = process.env.STEP;
if (!JOB) {
  console.error('usage: BASE=… JOB=<job-uuid> [STEP=<step-uuid>] node probe.mjs');
  process.exit(2);
}
const here = new URL('.', import.meta.url).pathname;
const out = (n) => `${here}probe-${n}.png`;
const log = (...a) => console.log('[uxprobe]', ...a);

// Guest session, minted outside the browser. The cookie arrives
// `Secure`; a plain-http tunnel origin would drop it, so it is
// re-injected non-Secure (README §How it authenticates).
const mint = await fetch(BASE + '/api/auth/guest', { method: 'POST' });
const cookieVal = (mint.headers.get('set-cookie') ?? '').match(/boss_session=([^;]+)/)?.[1];
if (!cookieVal) {
  console.error(`guest session refused (HTTP ${mint.status}) — is guest enabled on this deployment?`);
  process.exit(1);
}
log('guest session minted');

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
await ctx.addCookies([
  {
    name: 'boss_session',
    value: cookieVal,
    domain: new URL(BASE).hostname,
    path: '/',
    secure: false,
  },
]);
const page = await ctx.newPage();
page.on('pageerror', (e) => log('PAGEERROR:', String(e).slice(0, 300)));
const bad = [];
page.on('response', (r) => {
  if (r.status() >= 400) bad.push(`${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`);
});

const target = STEP ? `${BASE}/ux/jobs/${JOB}/steps/${STEP}` : `${BASE}/ux/jobs/${JOB}`;
await page.goto(target, { waitUntil: 'load', timeout: 30000 });
// Surfaces resolve their plugin lookup asynchronously — give the
// final render time to settle before judging it.
await page.waitForTimeout(3000);
await page.screenshot({ path: out('page'), fullPage: true });

// The step-focus body scrolls internally, so fullPage stops at the
// fold; walk the inner region and shoot each screenful.
const scroller = page.locator('.step-focus-body');
if (await scroller.count()) {
  const total = await scroller.evaluate((el) => el.scrollHeight);
  const view = await scroller.evaluate((el) => el.clientHeight);
  let shot = 0;
  for (let y = 0; y < total && shot < 8; y += Math.max(view - 60, 200)) {
    await scroller.evaluate((el, top) => { el.scrollTop = top; }, y);
    await page.waitForTimeout(250);
    await page.screenshot({ path: out(`scroll-${String(shot).padStart(2, '0')}`) });
    shot += 1;
  }
}

const text = await page.locator('body').innerText();
log('rendered text (first 1500):');
console.log(text.slice(0, 1500));

// The inventory that caught "one Start button and nothing else": what
// can the operator actually DO here?
const controls = await page.evaluate(() =>
  [...document.querySelectorAll('button, input, textarea, select')].map((e) => {
    const label =
      e.tagName === 'BUTTON'
        ? e.textContent?.trim().slice(0, 40)
        : e.getAttribute('placeholder') || e.getAttribute('aria-label') || e.getAttribute('type') || e.tagName;
    return `${e.tagName.toLowerCase()}:${label}${e.disabled ? ' (disabled)' : ''}`;
  }),
);
log('interactive controls:', JSON.stringify(controls));
if (bad.length) log('HTTP >= 400:', [...new Set(bad)].join(' | '));
await browser.close();
